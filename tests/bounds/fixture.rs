//! A Parquet file whose bounds can be worked out by hand, and the bounds
//! pipeline run on it the way the bench runs it
//!
//! 4,000 rows in 4 row groups of 1,000, data pages of 100 rows, no
//! dictionary, so every row group and page boundary is known:
//!
//! - `id`: the row number, so `id` ranges map straight to rows
//! - `val`: `(id % 100) * 40 + (id / 100) % 40`, a permutation of 0..4000
//!   where every page spans nearly the whole range, so `val` filters never
//!   prune and `val = v` matches exactly one row
//! - `name`: `n` plus the row number in five digits, sorted like `id`
//! - `price`: decimal(10, 2), `id + 0.50`, and null from row 3,000 on, so
//!   the last row group is all null
//! - `k`: 7 in every row, so statistics give its every value

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use datafusion::arrow::array::{Decimal128Array, Int64Array, RecordBatch, StringArray};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::parquet::arrow::ArrowWriter;
use datafusion::parquet::file::properties::{EnabledStatistics, WriterProperties};
use datafusion::prelude::{ParquetReadOptions, SessionContext};
use tokio::runtime::Runtime;

use crate::file::ParquetFile;
use crate::measure::{self, Bounds};
use crate::oracle;
use crate::scan::{self, DfScan, Scan};

pub(crate) const ROWS: i64 = 4000;
pub(crate) const ROW_GROUP_ROWS: usize = 1000;
pub(crate) const PAGE_ROWS: usize = 100;

/// Whether the file has a page index (column index and offset index)
#[derive(Clone, Copy)]
pub(crate) enum PageIndex {
    Yes,
    No,
}

/// `val` for row `id`
pub(crate) fn val(id: i64) -> i64 {
    (id % 100) * 40 + (id / 100) % 40
}

/// Writes the fixture as `<name>.parquet` in Cargo's temporary directory for
/// integration tests. Each test uses its own name, so tests running at once
/// never share a file
pub(crate) fn write(name: &str, index: PageIndex) -> PathBuf {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("val", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("price", DataType::Decimal128(10, 2), true),
        Field::new("k", DataType::Int64, false),
    ]));
    let ids: Vec<i64> = (0..ROWS).collect();
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(ids.clone())),
            Arc::new(Int64Array::from_iter_values(ids.iter().map(|&id| val(id)))),
            Arc::new(StringArray::from_iter_values(
                ids.iter().map(|id| format!("n{id:05}")),
            )),
            Arc::new(
                Decimal128Array::from_iter(
                    ids.iter()
                        .map(|&id| (id < 3000).then(|| i128::from(id) * 100 + 50)),
                )
                .with_precision_and_scale(10, 2)
                .expect("valid decimal type"),
            ),
            Arc::new(Int64Array::from(vec![7; ids.len()])),
        ],
    )
    .expect("columns match the schema");

    let statistics = match index {
        PageIndex::Yes => EnabledStatistics::Page,
        PageIndex::No => EnabledStatistics::Chunk,
    };
    let props = WriterProperties::builder()
        .set_max_row_group_row_count(Some(ROW_GROUP_ROWS))
        // Page size limits are checked between write batches
        .set_write_batch_size(PAGE_ROWS)
        .set_data_page_row_count_limit(PAGE_ROWS)
        .set_dictionary_enabled(false)
        .set_statistics_enabled(statistics)
        .set_offset_index_disabled(matches!(index, PageIndex::No))
        .build();

    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("bounds-{name}.parquet"));
    let file = File::create(&path).expect("create fixture file");
    let mut writer = ArrowWriter::try_new(file, schema, Some(props)).expect("open writer");
    writer.write(&batch).expect("write fixture rows");
    writer.close().expect("finish fixture file");
    path
}

/// A DataFusion session with the fixture registered as `t`
pub(crate) struct Session {
    pub(crate) runtime: Runtime,
    pub(crate) ctx: SessionContext,
    pub(crate) file: ParquetFile,
}

impl Session {
    pub(crate) fn new(path: &Path) -> Self {
        let runtime = Runtime::new().expect("tokio runtime");
        let ctx = SessionContext::new();
        runtime
            .block_on(ctx.register_parquet(
                "t",
                path.to_str().expect("UTF-8 temp path"),
                ParquetReadOptions::default(),
            ))
            .expect("register fixture");
        let file = ParquetFile::open(path).expect("read fixture metadata");
        Self { runtime, ctx, file }
    }

    /// The scans DataFusion plans for `sql`
    pub(crate) fn scans(&self, sql: &str) -> Vec<Scan> {
        self.runtime
            .block_on(scan::run(&self.ctx, sql))
            .unwrap_or_else(|err| panic!("planning `{sql}`: {err}"))
            .scans
    }

    /// The one scan DataFusion plans for `sql`
    pub(crate) fn scan(&self, sql: &str) -> Scan {
        let mut scans = self.scans(sql);
        assert_eq!(scans.len(), 1, "`{sql}` should scan `t` once");
        scans.remove(0)
    }

    /// Bounds for the one scan of `sql`, with the data pass, as the bench runs them
    pub(crate) fn bounds(&self, sql: &str) -> Bounds {
        let scan = self.scan(sql);
        let mask = (!scan.filters.is_empty()).then(|| {
            oracle::matches(&self.ctx, &scan, &scan.filter_columns(), self.file.rows())
                .unwrap_or_else(|err| panic!("data pass for `{sql}`: {err}"))
        });
        measure::bound(&scan, &self.file, mask.as_ref())
            .unwrap_or_else(|err| panic!("bounds for `{sql}`: {err}"))
    }

    /// DataFusion's own pruning for the one scan of `sql`, run alone
    pub(crate) fn datafusion(&self, sql: &str) -> DfScan {
        let scan = self.scan(sql);
        self.runtime
            .block_on(scan::datafusion_scan(&self.ctx, &scan))
            .unwrap_or_else(|err| panic!("DataFusion scan for `{sql}`: {err}"))
    }
}
