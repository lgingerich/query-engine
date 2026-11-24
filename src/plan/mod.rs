pub(crate) mod errors;

use crate::ParquetSource;
use crate::bind::BoundFilter;
use crate::bind::BoundQuery;
use crate::bind::ScalarValue;
use crate::plan::errors::PlanError;
use crate::query::CompareOp;
use parquet::file::metadata::RowGroupMetaData;
use parquet::file::statistics::Statistics;
/*
3. plan(scan_request) -> ScanPlan
    using Parquet metadata, plan out what row groups and data we keep
    no io or execution here, only checks against metadata

At this point, we've only read the footer. We haven't done any IO. Just want the full scope of things to pass into the parquet read for pruning.



## What to take from the SQL and hand to the scan

Example: `SELECT name FROM t WHERE age > 30 AND city = 'Austin' LIMIT 10`

| From SQL | Example | Used for |
| --- | --- | --- |
| Columns needed | `name`, `age`, `city` | **Projection:** never read any other column |
| Simple filters (column op constant) | `age > 30`, `city = 'Austin'` | **Pruning:** skip row groups and pages |
| Limit | `10` | Stop reading early. Only safe when there's no ORDER BY or aggregation after the scan |

Complex filters such as `upper(city) = 'AUSTIN'` or `age > other_col` can't be pushed down. Keep them for row-by-row filtering.



*/

#[derive(Debug)]
pub(crate) struct ScanPlan {
    /// Projection, residual filters, limit, and output schema from bind.
    query: BoundQuery,
    /// Table column indexes to decode: the projection, plus any filter column it does not already include.
    read_columns: Vec<usize>,
    /// Row groups that survived footer and bloom-filter pruning, in file order.
    row_groups: Vec<PlannedRowGroup>,
}

#[derive(Debug)]
pub(crate) struct PlannedRowGroup {
    /// Index into the file's row groups.
    index: usize,
    /// Half-open row offsets within this row group, in increasing order.
    rows: Vec<RowRange>,
}

#[derive(Debug)]
pub(crate) struct RowRange {
    start: u64,
    end: u64,
}

pub(crate) fn plan(bound_query: BoundQuery, source: &ParquetSource) -> Result<ScanPlan, PlanError> {
    // Get column indexes from `BoundFilter` then combine with projection column indexes into `read_columns`
    let filter_columns: Vec<_> = bound_query
        .filters
        .iter()
        .map(|filter| filter.column)
        .collect();
    let mut read_columns = bound_query.projection.clone();
    for &column in &filter_columns {
        if !read_columns.contains(&column) {
            read_columns.push(column);
        }
    }

    let metadata = source.metadata();
    let mut row_groups = Vec::with_capacity(metadata.num_row_groups());
    for (index, row_group) in metadata.row_groups().iter().enumerate() {
        // skip row group if filters can't match
        if !bound_query
            .filters
            .iter()
            .all(|filter| row_group_may_match(row_group, filter))
        {
            continue;
        }

        let num_rows = row_group.num_rows();
        let end = match u64::try_from(num_rows) {
            Ok(0) => continue, // empty row group, nothing to read
            Ok(end) => end,
            Err(_) => {
                return Err(PlanError::InvalidRowCount {
                    row_group: index,
                    num_rows,
                });
            }
        };

        // row ranges are relative to each row group, therefore start is always 0
        row_groups.push(PlannedRowGroup {
            index,
            rows: vec![RowRange { start: 0, end }],
        });
    }

    Ok(ScanPlan {
        query: bound_query,
        read_columns,
        row_groups,
    })
}

/// Whether footer stats leave any chance that `filter` matches a row in this
/// row group. Errs toward `true`: missing stats or an unhandled type keep it
fn row_group_may_match(row_group: &RowGroupMetaData, filter: &BoundFilter) -> bool {
    let Some(stats) = row_group.column(filter.column).statistics() else {
        return true;
    };
    // All nulls: no comparison is ever true
    if stats
        .null_count_opt()
        .is_some_and(|nulls| u64::try_from(row_group.num_rows()) == Ok(nulls))
    {
        return false;
    }
    // Truncated bounds still bound the values, except for `<>`, which needs exact min == max
    if filter.op == CompareOp::NotEq && !(stats.min_is_exact() && stats.max_is_exact()) {
        return true;
    }

    let op = filter.op;
    match (stats, &filter.value) {
        (Statistics::Int32(s), ScalarValue::Int32(v) | ScalarValue::Date32(v)) => {
            may_match(op, v, s.min_opt(), s.max_opt())
        }
        // INT32-stored decimal: widen bounds to the bound value's i64
        (Statistics::Int32(s), ScalarValue::Decimal(v)) => {
            let min = s.min_opt().map(|&m| i64::from(m));
            let max = s.max_opt().map(|&m| i64::from(m));
            may_match(op, v, min.as_ref(), max.as_ref())
        }
        (Statistics::Int64(s), ScalarValue::Int64(v) | ScalarValue::Decimal(v)) => {
            may_match(op, v, s.min_opt(), s.max_opt())
        }
        // Legacy min/max fields were written with signed byte order, wrong for strings
        (Statistics::ByteArray(s), ScalarValue::Utf8(v)) if !stats.is_min_max_deprecated() => {
            may_match(op, v.as_bytes(), s.min_bytes_opt(), s.max_bytes_opt())
        }
        _ => true,
    }
}

/// Whether some value in `[min, max]` can satisfy `x op v`. A `None` bound is unknown
fn may_match<T: PartialOrd + ?Sized>(
    op: CompareOp,
    v: &T,
    min: Option<&T>,
    max: Option<&T>,
) -> bool {
    match op {
        CompareOp::Eq => !(min.is_some_and(|m| v < m) || max.is_some_and(|m| v > m)),
        CompareOp::NotEq => !(min.is_some_and(|m| m == v) && max.is_some_and(|m| m == v)),
        CompareOp::Lt => !min.is_some_and(|m| m >= v),
        CompareOp::LtEq => !min.is_some_and(|m| m > v),
        CompareOp::Gt => !max.is_some_and(|m| m <= v),
        CompareOp::GtEq => !max.is_some_and(|m| m < v),
    }
}
