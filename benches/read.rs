//! Compare ways of reading the TPC-H Parquet files.
//!
//! * `encoded` — decompress each page and keep the encoded buffer. Batch size
//!   does not apply; the argument is ignored.
//! * `plain` — decode to physical column vectors. Batch size is the
//!   `read_records` chunk.
//! * `arrow` — decode to Arrow `RecordBatch`es. Batch size is `with_batch_size`.
//! * `arrow-dict` — `arrow`, with dictionary-encoded byte arrays kept as
//!   `Dictionary(Int32, Utf8/Binary)`.
//! * `arrow-view` — `arrow`, with `Utf8`/`Binary` read as view types.
//!
//! The decoded result is kept until process exit. `peak_rss_bytes` is the
//! process's peak resident memory as reported by the OS (`getrusage`) after
//! the read. It includes transient decode buffers, metadata and allocator
//! overhead as well as the kept result, so it is an upper bound on the
//! retained working set, not the working set itself. It is also per process
//! and never goes down: with several files, each line reports the peak so far.
//! Run one file and one mode per process when comparing memory.
//!
//! ```text
//! cargo bench --features bench --bench read -- arrow 65536 tpch-data/lineitem.parquet
//! ```

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use arrow_schema::{DataType, FieldRef, Schema, SchemaRef};
use parquet::arrow::arrow_reader::{
    ArrowReaderMetadata, ArrowReaderOptions, ParquetRecordBatchReaderBuilder,
};
use parquet::basic::Encoding;
use parquet::column::page::Page;
use parquet::column::reader::{ColumnReader, ColumnReaderImpl};
use parquet::data_type::{ByteArray, DataType as ParquetDataType, FixedLenByteArray, Int96};
use parquet::errors::{ParquetError, Result};
use parquet::file::metadata::ParquetMetaData;
use parquet::file::reader::{FileReader, SerializedFileReader};

#[derive(Clone, Copy)]
enum ArrowKind {
    Plain,
    Dict,
    View,
}

enum Held {
    Bool(Vec<bool>),
    I32(Vec<i32>),
    I64(Vec<i64>),
    I96(Vec<Int96>),
    F32(Vec<f32>),
    F64(Vec<f64>),
    Bytes(Vec<ByteArray>),
    Fixed(Vec<FixedLenByteArray>),
}

struct Run {
    rows: usize,
    elapsed_ms: u128,
    detail: String,
    hold: Box<dyn std::any::Any>,
}

fn main() -> Result<()> {
    // `cargo nextest` lists tests with `--list`; this target has none.
    if std::env::args().any(|arg| arg == "--list") {
        return Ok(());
    }
    let mut args = std::env::args().skip(1);
    let mode = args.next().ok_or_else(usage)?;
    let batch: usize = args
        .next()
        .ok_or_else(usage)?
        .parse()
        .map_err(|err: std::num::ParseIntError| ParquetError::General(err.to_string()))?;
    if batch == 0 {
        return Err(ParquetError::General("batch size must be > 0".to_string()));
    }
    // The test runner appends `--bench` when `cargo bench` launches this target.
    let files = parquet_files(args.filter(|arg| arg != "--bench"))?;
    if files.is_empty() {
        return Err(ParquetError::General("no parquet files".to_string()));
    }

    if mode == "schema" {
        for path in &files {
            dump_schema(path)?;
        }
        return Ok(());
    }

    let mut kept = Vec::new();
    for path in &files {
        let run = match mode.as_str() {
            "encoded" => load_encoded(path)?,
            "plain" => load_plain(path, batch)?,
            "arrow" => load_arrow(path, batch, ArrowKind::Plain)?,
            "arrow-dict" => load_arrow(path, batch, ArrowKind::Dict)?,
            "arrow-view" => load_arrow(path, batch, ArrowKind::View)?,
            other => {
                return Err(ParquetError::General(format!(
                    "unknown mode `{other}` (expected encoded, plain, arrow, arrow-dict, arrow-view, schema)"
                )));
            }
        };
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| ParquetError::General(format!("bad path {}", path.display())))?;
        let batch_label = if mode == "encoded" {
            "na".to_string()
        } else {
            batch.to_string()
        };
        emit(&format!(
            "RESULT\tmode={mode}\tbatch={batch_label}\tfile={file_name}\trows={rows}\telapsed_ms={elapsed}\tpeak_rss_bytes={peak}\t{detail}",
            rows = run.rows,
            elapsed = run.elapsed_ms,
            peak = peak_rss_bytes()?,
            detail = run.detail,
        ))?;
        kept.push(run.hold);
    }
    std::hint::black_box(&kept);
    Ok(())
}

/// Peak resident set size of this process so far, in bytes, from
/// `getrusage(RUSAGE_SELF)`. A high-water mark: it never goes down.
#[expect(
    unsafe_code,
    reason = "getrusage is a C call with no safe std wrapper; the crate otherwise denies unsafe"
)]
fn peak_rss_bytes() -> Result<u64> {
    // SAFETY: `rusage` is a plain C struct of integers, for which all-zero bytes are valid.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `usage` is a valid, writable `rusage` that outlives the call.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &raw mut usage) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let max = u64::try_from(usage.ru_maxrss).unwrap_or(0);
    // Apple platforms report `ru_maxrss` in bytes; Linux and the BSDs report KiB.
    Ok(if cfg!(target_vendor = "apple") {
        max
    } else {
        max * 1024
    })
}

fn emit(line: &str) -> Result<()> {
    use std::io::Write;
    writeln!(std::io::stdout(), "{line}")?;
    Ok(())
}

fn usage() -> ParquetError {
    ParquetError::General(
        "usage: cargo bench --features bench --bench read -- <encoded|plain|arrow|arrow-dict|arrow-view|schema> <batch_rows> [files-or-dirs...]"
            .to_string(),
    )
}

fn parquet_files(args: impl Iterator<Item = String>) -> Result<Vec<PathBuf>> {
    let args: Vec<String> = args.collect();
    let mut paths = Vec::new();
    if args.is_empty() {
        collect_parquet(Path::new("tpch-data"), &mut paths)?;
    } else {
        for arg in &args {
            let path = PathBuf::from(arg);
            if path.is_dir() {
                collect_parquet(&path, &mut paths)?;
            } else {
                paths.push(path);
            }
        }
    }
    paths.sort();
    Ok(paths)
}

fn collect_parquet(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("parquet") {
            out.push(path);
        }
    }
    Ok(())
}

fn load_encoded(path: &Path) -> Result<Run> {
    let started = Instant::now();
    let reader = SerializedFileReader::new(File::open(path)?)?;
    let meta = reader.metadata();
    let rows = file_rows(meta)?;
    let mut pages: Vec<Page> = Vec::new();
    let mut dict_pages = 0u64;
    for rg in 0..meta.num_row_groups() {
        let row_group = reader.get_row_group(rg)?;
        for col in 0..row_group.num_columns() {
            for page in row_group.get_column_page_reader(col)? {
                let page = page?;
                if page.is_dictionary_page() {
                    dict_pages += 1;
                }
                pages.push(page);
            }
        }
    }
    let elapsed_ms = started.elapsed().as_millis();
    let detail = format!(
        "pages={pages}\tdict_pages={dict_pages}\tfile_bytes={file_bytes}\tuncompressed_bytes={uncompressed}",
        pages = pages.len(),
        file_bytes = file_len(path)?,
        uncompressed = uncompressed_bytes(meta),
    );
    Ok(Run {
        rows,
        elapsed_ms,
        detail,
        hold: Box::new(pages),
    })
}

macro_rules! read_phys {
    ($columns:expr, $idx:expr, $rows:expr, $max_def:expr, $batch:expr, $reader:expr, $variant:ident) => {{
        let slot = $columns[$idx].get_or_insert_with(|| Held::$variant(Vec::with_capacity($rows)));
        let Held::$variant(values) = slot else {
            return Err(ParquetError::General(
                "physical type changed across row groups".to_string(),
            ));
        };
        read_column(&mut $reader, $max_def, $batch, values)?
    }};
}

fn load_plain(path: &Path, batch: usize) -> Result<Run> {
    let started = Instant::now();
    let reader = SerializedFileReader::new(File::open(path)?)?;
    let meta = reader.metadata();
    let rows = file_rows(meta)?;
    let ncols = meta.file_metadata().schema_descr().num_columns();
    let mut columns: Vec<Option<Held>> = Vec::new();
    columns.resize_with(ncols, || None);
    let mut seen = vec![0usize; ncols];

    for rg in 0..meta.num_row_groups() {
        let row_group = reader.get_row_group(rg)?;
        for col in 0..ncols {
            let descr = row_group.metadata().schema_descr().column(col);
            if descr.max_rep_level() > 0 {
                return Err(ParquetError::NYI(format!("nested column {}", descr.name())));
            }
            let max_def = descr.max_def_level();
            seen[col] += match row_group.get_column_reader(col)? {
                ColumnReader::BoolColumnReader(mut r) => {
                    read_phys!(columns, col, rows, max_def, batch, r, Bool)
                }
                ColumnReader::Int32ColumnReader(mut r) => {
                    read_phys!(columns, col, rows, max_def, batch, r, I32)
                }
                ColumnReader::Int64ColumnReader(mut r) => {
                    read_phys!(columns, col, rows, max_def, batch, r, I64)
                }
                ColumnReader::Int96ColumnReader(mut r) => {
                    read_phys!(columns, col, rows, max_def, batch, r, I96)
                }
                ColumnReader::FloatColumnReader(mut r) => {
                    read_phys!(columns, col, rows, max_def, batch, r, F32)
                }
                ColumnReader::DoubleColumnReader(mut r) => {
                    read_phys!(columns, col, rows, max_def, batch, r, F64)
                }
                ColumnReader::ByteArrayColumnReader(mut r) => {
                    read_phys!(columns, col, rows, max_def, batch, r, Bytes)
                }
                ColumnReader::FixedLenByteArrayColumnReader(mut r) => {
                    read_phys!(columns, col, rows, max_def, batch, r, Fixed)
                }
            };
        }
    }
    let elapsed_ms = started.elapsed().as_millis();

    for (col, column) in columns.iter().enumerate() {
        if seen[col] != rows {
            return Err(ParquetError::General(format!(
                "plain reader produced {} records in column {col}, file has {rows} rows",
                seen[col]
            )));
        }
        if column.is_none() {
            return Err(ParquetError::General(format!("column {col} was not read")));
        }
    }
    let detail = format!(
        "file_bytes={file_bytes}\tuncompressed_bytes={uncompressed}",
        file_bytes = file_len(path)?,
        uncompressed = uncompressed_bytes(meta),
    );
    Ok(Run {
        rows,
        elapsed_ms,
        detail,
        hold: Box::new(columns),
    })
}

fn read_column<T: ParquetDataType>(
    reader: &mut ColumnReaderImpl<T>,
    max_def_level: i16,
    batch: usize,
    values: &mut Vec<T::T>,
) -> Result<usize> {
    let mut defs = Vec::new();
    let mut records = 0usize;
    loop {
        defs.clear();
        let (n, _, _) = reader.read_records(
            batch,
            (max_def_level > 0).then_some(&mut defs),
            None,
            values,
        )?;
        if n == 0 {
            return Ok(records);
        }
        records += n;
    }
}

fn load_arrow(path: &Path, batch: usize, kind: ArrowKind) -> Result<Run> {
    let started = Instant::now();
    let file = File::open(path)?;
    // These files embed an Arrow schema that already uses `Utf8View`. Override
    // it so the three arrow modes are actually different representations.
    let probed = ArrowReaderMetadata::load(&file, ArrowReaderOptions::new())?;
    let rows_expected = file_rows(probed.metadata())?;
    let uncompressed = uncompressed_bytes(probed.metadata());
    let (schema, rewritten) = rewrite_schema(kind, probed.schema(), probed.metadata())?;
    let options = ArrowReaderOptions::new().with_schema(schema);
    let metadata = ArrowReaderMetadata::try_new(Arc::clone(probed.metadata()), options)?;
    let reader = ParquetRecordBatchReaderBuilder::new_with_metadata(file, metadata)
        .with_batch_size(batch)
        .build()?;
    let file_bytes = file_len(path)?;
    let mut batches = Vec::new();
    let mut rows = 0usize;
    for record_batch in reader {
        let record_batch = record_batch?;
        rows += record_batch.num_rows();
        batches.push(record_batch);
    }
    let elapsed_ms = started.elapsed().as_millis();
    if rows != rows_expected {
        return Err(ParquetError::General(format!(
            "arrow reader produced {rows} rows, file has {rows_expected} rows"
        )));
    }
    let detail = format!(
        "batches={n}\trewritten={rewritten}\tfile_bytes={file_bytes}\tuncompressed_bytes={uncompressed}",
        n = batches.len(),
    );
    Ok(Run {
        rows,
        elapsed_ms,
        detail,
        hold: Box::new(batches),
    })
}

fn rewrite_schema(
    kind: ArrowKind,
    schema: &SchemaRef,
    meta: &ParquetMetaData,
) -> Result<(SchemaRef, usize)> {
    let ncols = meta.file_metadata().schema_descr().num_columns();
    if schema.fields().len() != ncols {
        return Err(ParquetError::NYI(
            "schema rewrite expects one arrow field per physical column".to_string(),
        ));
    }
    let mut rewritten = 0usize;
    let mut fields = Vec::with_capacity(ncols);
    for (index, field) in schema.fields().iter().enumerate() {
        let next = match kind {
            ArrowKind::Plain if let Some(owned) = owned_byte_type(field.data_type()) => {
                retarget(field, owned, &mut rewritten)
            }
            ArrowKind::View if let Some(view) = view_byte_type(field.data_type()) => {
                retarget(field, view, &mut rewritten)
            }
            ArrowKind::Dict
                if dict_encoded(meta, index) && owned_byte_type(field.data_type()).is_some() =>
            {
                rewritten += 1;
                dictionary_field(field)
            }
            ArrowKind::Plain | ArrowKind::Dict | ArrowKind::View => Arc::clone(field),
        };
        fields.push(next);
    }
    Ok((
        Arc::new(Schema::new_with_metadata(fields, schema.metadata().clone())),
        rewritten,
    ))
}

fn retarget(field: &FieldRef, dtype: DataType, rewritten: &mut usize) -> FieldRef {
    if field.data_type() == &dtype {
        return Arc::clone(field);
    }
    *rewritten += 1;
    Arc::new(field.as_ref().clone().with_data_type(dtype))
}

fn dictionary_field(field: &FieldRef) -> FieldRef {
    let value = match field.data_type() {
        DataType::Binary | DataType::LargeBinary | DataType::BinaryView => DataType::Binary,
        _ => DataType::Utf8,
    };
    Arc::new(field.as_ref().clone().with_data_type(DataType::Dictionary(
        Box::new(DataType::Int32),
        Box::new(value),
    )))
}

fn owned_byte_type(dtype: &DataType) -> Option<DataType> {
    match dtype {
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => Some(DataType::Utf8),
        DataType::Binary | DataType::LargeBinary | DataType::BinaryView => Some(DataType::Binary),
        _ => None,
    }
}

fn view_byte_type(dtype: &DataType) -> Option<DataType> {
    match dtype {
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => Some(DataType::Utf8View),
        DataType::Binary | DataType::LargeBinary | DataType::BinaryView => {
            Some(DataType::BinaryView)
        }
        _ => None,
    }
}

fn dict_encoded(meta: &ParquetMetaData, col: usize) -> bool {
    !meta.row_groups().is_empty()
        && meta.row_groups().iter().all(|group| {
            group
                .column(col)
                .encodings()
                .any(|enc| matches!(enc, Encoding::RLE_DICTIONARY | Encoding::PLAIN_DICTIONARY))
        })
}

fn dump_schema(path: &Path) -> Result<()> {
    use std::fmt::Write;
    let reader = SerializedFileReader::new(File::open(path)?)?;
    let meta = reader.metadata();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("?");
    emit(&format!(
        "FILE\t{file_name}\trows={rows}\trow_groups={groups}\tfile_bytes={file_bytes}\tuncompressed_bytes={uncompressed}",
        rows = file_rows(meta)?,
        groups = meta.num_row_groups(),
        file_bytes = file_len(path)?,
        uncompressed = uncompressed_bytes(meta),
    ))?;
    if meta.num_row_groups() == 0 {
        return Ok(());
    }
    let group = meta.row_group(0);
    for (index, col) in meta
        .file_metadata()
        .schema_descr()
        .columns()
        .iter()
        .enumerate()
    {
        let chunk = group.column(index);
        let mut encs = String::new();
        for (n, enc) in chunk.encodings().enumerate() {
            if n > 0 {
                encs.push('+');
            }
            let _ = write!(encs, "{enc:?}");
        }
        emit(&format!(
            "SCHEMA\t{file_name}\tcol={index}\tname={name}\tphysical={physical:?}\tlogical={logical:?}\tcompression={compression}\tencodings={encs}",
            name = col.name(),
            physical = col.physical_type(),
            logical = col.logical_type_ref(),
            compression = chunk.compression(),
        ))?;
    }
    let file = File::open(path)?;
    let arrow_meta = ArrowReaderMetadata::load(&file, ArrowReaderOptions::new())?;
    let mut types = String::new();
    for (index, field) in arrow_meta.schema().fields().iter().enumerate() {
        if index > 0 {
            types.push(',');
        }
        let _ = write!(types, "{}:{}", field.name(), field.data_type());
    }
    emit(&format!("ARROW\t{file_name}\t{types}"))?;
    Ok(())
}

fn file_rows(meta: &ParquetMetaData) -> Result<usize> {
    usize::try_from(meta.file_metadata().num_rows())
        .map_err(|err| ParquetError::General(err.to_string()))
}

fn uncompressed_bytes(meta: &ParquetMetaData) -> i64 {
    meta.row_groups()
        .iter()
        .map(parquet::file::metadata::RowGroupMetaData::total_byte_size)
        .sum()
}

fn file_len(path: &Path) -> Result<u64> {
    Ok(std::fs::metadata(path)?.len())
}
