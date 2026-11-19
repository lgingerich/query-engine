//! The data pass: which rows of a scan pass its filters
//!
//! Reads only the filter columns and evaluates the filters with DataFusion's
//! expressions, so every filter DataFusion can run is covered, `LIKE` and
//! functions included.

use std::collections::BTreeSet;
use std::error::Error;
use std::fs::File;
use std::sync::Arc;

use datafusion::arrow::array::{Array, AsArray};
use datafusion::common::DFSchema;
use datafusion::logical_expr::utils::conjunction;
use datafusion::parquet::arrow::ProjectionMask;
use datafusion::parquet::arrow::arrow_reader::{
    ArrowReaderOptions, ParquetRecordBatchReaderBuilder,
};
use datafusion::prelude::SessionContext;

use crate::scan::Scan;

const BATCH_ROWS: usize = 64 * 1024;

/// One bit per row of the file, set for rows that pass the scan's filters
pub(crate) struct Mask {
    words: Vec<u64>,
}

impl Mask {
    /// No rows set, for a file of `rows` rows
    pub(crate) fn new(rows: u64) -> Self {
        let words = usize::try_from(rows.div_ceil(64)).unwrap_or(0);
        Self {
            words: vec![0; words],
        }
    }

    /// Marks `row`. Rows past the end are ignored
    pub(crate) fn set(&mut self, row: u64) {
        let (word, bit) = (row / 64, row % 64);
        if let Some(word) = usize::try_from(word)
            .ok()
            .and_then(|w| self.words.get_mut(w))
        {
            *word |= 1 << bit;
        }
    }

    /// Set bits in rows `[start, end)`
    pub(crate) fn count(&self, start: u64, end: u64) -> u64 {
        let mut total = 0;
        let mut from = start;
        while from < end {
            // One word at a time: from `from` to the word's end or `end`
            let to = ((from / 64 + 1) * 64).min(end);
            let word = usize::try_from(from / 64)
                .ok()
                .and_then(|w| self.words.get(w))
                .copied()
                .unwrap_or(0);
            let width = to - from;
            let bits = if width == 64 {
                word
            } else {
                (word >> (from % 64)) & ((1u64 << width) - 1)
            };
            total += u64::from(bits.count_ones());
            from = to;
        }
        total
    }

    pub(crate) fn any(&self, start: u64, end: u64) -> bool {
        self.count(start, end) > 0
    }
}

/// Rows of the scan's file that pass all of its filters
pub(crate) fn matches(
    ctx: &SessionContext,
    scan: &Scan,
    filter_columns: &BTreeSet<String>,
    rows: u64,
) -> Result<Mask, Box<dyn Error>> {
    let Some(filter) = conjunction(scan.filters.iter().cloned()) else {
        return Err("scan has no filters".into());
    };
    let file = File::open(&scan.path)?;
    let options = ArrowReaderOptions::new().with_schema(Arc::clone(&scan.schema));
    let builder = ParquetRecordBatchReaderBuilder::try_new_with_options(file, options)?;
    let leaves = filter_columns
        .iter()
        .map(|name| {
            builder
                .parquet_schema()
                .columns()
                .iter()
                .position(|column| column.path().parts() == [name.as_str()])
                .ok_or_else(|| format!("filter column `{name}` not in the file"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let projection = ProjectionMask::leaves(builder.parquet_schema(), leaves);
    let reader = builder
        .with_projection(projection)
        .with_batch_size(BATCH_ROWS)
        .build()?;

    let mut mask = Mask::new(rows);
    let mut predicate = None;
    let mut offset = 0u64;
    for batch in reader {
        let batch = batch?;
        if predicate.is_none() {
            let schema = DFSchema::try_from(batch.schema().as_ref().clone())?;
            predicate = Some(ctx.state().create_physical_expr(filter.clone(), &schema)?);
        }
        let Some(predicate) = &predicate else {
            break;
        };
        let values = predicate.evaluate(&batch)?.into_array(batch.num_rows())?;
        let values = values
            .as_boolean_opt()
            .ok_or("filters did not produce booleans")?;
        for i in 0..values.len() {
            if values.is_valid(i) && values.value(i) {
                mask.set(offset + u64::try_from(i).unwrap_or(0));
            }
        }
        offset += u64::try_from(batch.num_rows()).unwrap_or(0);
    }
    if offset != rows {
        return Err(format!("read {offset} rows, file has {rows}").into());
    }
    Ok(mask)
}
