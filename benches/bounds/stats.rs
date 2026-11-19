//! Min/max ranges from Parquet statistics, and the first-principles test of
//! whether a filter can match any row in a range.
//!
//! Everything compares physical values: integers (dates as days, decimals
//! unscaled at the column's scale) as `i128`, and strings as bytes, which is
//! Parquet's sort order for both. A test returns "empty" only when the range
//! proves it; anything it doesn't understand keeps the rows.
//!
//! Min and max are treated as bounds, never as values that occur: Parquet
//! allows writers to store looser bounds (truncated strings, for one). So
//! nothing here needs them to be exact. Where min equals max, every non-null
//! value lies between them and so equals them.

use std::collections::HashMap;

use datafusion::arrow::datatypes::{DataType, Field};
use datafusion::logical_expr::{Expr, Operator};
use datafusion::parquet::basic::Type as Physical;
use datafusion::parquet::file::page_index::column_index::ColumnIndexMetaData;
use datafusion::parquet::file::statistics::Statistics;
use datafusion::scalar::ScalarValue;

/// A physical value, comparable only with values of the same column
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Val {
    Int(i128),
    Bytes(Vec<u8>),
}

/// How a column's statistics compare
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Signed integers in INT32 or INT64: ints, dates, and decimals at `scale`
    Int { scale: i8 },
    /// `BYTE_ARRAY` strings and binary, compared as unsigned bytes
    Bytes,
    /// Anything else: its statistics are never used
    Other,
}

impl Kind {
    pub(crate) fn of(field: &Field, physical: Physical) -> Self {
        use DataType as D;
        match (field.data_type(), physical) {
            // Unsigned 32- and 64-bit values sort differently from their
            // signed storage, so they're left out
            (
                D::Int8 | D::Int16 | D::Int32 | D::Int64 | D::UInt8 | D::UInt16 | D::Date32,
                Physical::INT32 | Physical::INT64,
            ) => Self::Int { scale: 0 },
            (D::Decimal128(_, scale), Physical::INT32 | Physical::INT64) => {
                Self::Int { scale: *scale }
            }
            (
                D::Utf8 | D::LargeUtf8 | D::Utf8View | D::Binary | D::LargeBinary | D::BinaryView,
                Physical::BYTE_ARRAY,
            ) => Self::Bytes,
            _ => Self::Other,
        }
    }
}

/// What statistics say about some rows: a column chunk or a page
#[derive(Clone, Debug, Default)]
pub(crate) struct Range {
    /// Lower bound of the non-null values
    pub(crate) min: Option<Val>,
    /// Upper bound of the non-null values
    pub(crate) max: Option<Val>,
    pub(crate) nulls: Option<u64>,
    pub(crate) rows: u64,
}

impl Range {
    /// From a column chunk's statistics
    pub(crate) fn of_chunk(stats: Option<&Statistics>, kind: Kind, rows: u64) -> Self {
        let nulls = stats.and_then(Statistics::null_count_opt);
        let int = |min: Option<i128>, max: Option<i128>| Self {
            min: min.map(Val::Int),
            max: max.map(Val::Int),
            nulls,
            rows,
        };
        match (kind, stats) {
            (Kind::Int { .. }, Some(Statistics::Int32(s))) => int(
                s.min_opt().map(|v| i128::from(*v)),
                s.max_opt().map(|v| i128::from(*v)),
            ),
            (Kind::Int { .. }, Some(Statistics::Int64(s))) => int(
                s.min_opt().map(|v| i128::from(*v)),
                s.max_opt().map(|v| i128::from(*v)),
            ),
            // The deprecated min/max fields may use the wrong sort order for bytes
            (Kind::Bytes, Some(all @ Statistics::ByteArray(s))) if !all.is_min_max_deprecated() => {
                Self {
                    min: s.min_opt().map(|v| Val::Bytes(v.data().to_vec())),
                    max: s.max_opt().map(|v| Val::Bytes(v.data().to_vec())),
                    nulls,
                    rows,
                }
            }
            _ => Self {
                nulls,
                rows,
                ..Self::default()
            },
        }
    }

    /// From page `page` of a column index
    pub(crate) fn of_page(index: &ColumnIndexMetaData, page: usize, kind: Kind, rows: u64) -> Self {
        let nulls = index
            .null_count(page)
            .and_then(|count| u64::try_from(count).ok());
        if index.is_null_page(page) {
            return Self {
                nulls: Some(rows),
                rows,
                ..Self::default()
            };
        }
        let (min, max) = match (kind, index) {
            (Kind::Int { .. }, ColumnIndexMetaData::INT32(x)) => (
                x.min_value(page).map(|v| Val::Int(i128::from(*v))),
                x.max_value(page).map(|v| Val::Int(i128::from(*v))),
            ),
            (Kind::Int { .. }, ColumnIndexMetaData::INT64(x)) => (
                x.min_value(page).map(|v| Val::Int(i128::from(*v))),
                x.max_value(page).map(|v| Val::Int(i128::from(*v))),
            ),
            // Truncated for long strings: still bounds
            (Kind::Bytes, ColumnIndexMetaData::BYTE_ARRAY(x)) => (
                x.min_value(page).map(|v| Val::Bytes(v.to_vec())),
                x.max_value(page).map(|v| Val::Bytes(v.to_vec())),
            ),
            _ => (None, None),
        };
        Self {
            min,
            max,
            nulls,
            rows,
        }
    }

    fn all_null(&self) -> bool {
        self.rows > 0 && self.nulls == Some(self.rows)
    }

    /// Whether the statistics give every value without reading any: all equal
    /// one value (bounds that meet, no nulls), or all null. Such rows cost
    /// nothing to read; DataFusion, for one, turns such a column into a constant
    pub(crate) fn values_known(&self) -> bool {
        let one_value = self.nulls == Some(0) && self.min.is_some() && self.min == self.max;
        self.rows > 0 && (one_value || self.all_null())
    }
}

/// Column kinds and the statistics for some rows, by column name
pub(crate) struct Ctx<'a> {
    pub(crate) kinds: &'a HashMap<String, Kind>,
    pub(crate) range: &'a dyn Fn(&str) -> Option<Range>,
}

/// Whether `expr` can't be true for any row the ranges describe
pub(crate) fn proves_empty(expr: &Expr, ctx: &Ctx<'_>) -> bool {
    match expr {
        Expr::BinaryExpr(b) => match b.op {
            Operator::And => proves_empty(&b.left, ctx) || proves_empty(&b.right, ctx),
            Operator::Or => proves_empty(&b.left, ctx) && proves_empty(&b.right, ctx),
            op => comparison(&b.left, op, &b.right).is_some_and(|(column, op, value)| {
                leaf(ctx, column, &[value]).is_some_and(|(range, values)| {
                    range.all_null() || compare_empty(&range, op, &values[0])
                })
            }),
        },
        Expr::Between(b) if !b.negated => {
            match (column(&b.expr), literal(&b.low), literal(&b.high)) {
                (Some(column), Some(low), Some(high)) => leaf(ctx, column, &[low, high])
                    .is_some_and(|(range, values)| {
                        range.all_null()
                            || range.max.as_ref().is_some_and(|max| *max < values[0])
                            || range.min.as_ref().is_some_and(|min| *min > values[1])
                    }),
                _ => false,
            }
        }
        Expr::InList(list) if !list.negated => {
            let values: Option<Vec<_>> = list.list.iter().map(literal).collect();
            match (column(&list.expr), values) {
                (Some(column), Some(values)) => {
                    leaf(ctx, column, &values).is_some_and(|(range, values)| {
                        range.all_null()
                            || values
                                .iter()
                                .all(|value| compare_empty(&range, Operator::Eq, value))
                    })
                }
                _ => false,
            }
        }
        Expr::IsNull(inner) => column(inner)
            .and_then(|column| (ctx.range)(column))
            .is_some_and(|range| range.nulls == Some(0)),
        Expr::IsNotNull(inner) => column(inner)
            .and_then(|column| (ctx.range)(column))
            .is_some_and(|range| range.all_null()),
        _ => false,
    }
}

/// Whether statistics can ever rule rows out for `expr`: it has a supported
/// comparison on a supported column, through `AND` (either side) or `OR` (both)
pub(crate) fn usable(expr: &Expr, kinds: &HashMap<String, Kind>) -> bool {
    let supported = |column: &str, values: &[&ScalarValue]| {
        kinds.get(column).is_some_and(|kind| {
            *kind != Kind::Other && values.iter().all(|value| to_val(value, *kind).is_some())
        })
    };
    match expr {
        Expr::BinaryExpr(b) => match b.op {
            Operator::And => usable(&b.left, kinds) || usable(&b.right, kinds),
            Operator::Or => usable(&b.left, kinds) && usable(&b.right, kinds),
            op => comparison(&b.left, op, &b.right)
                .is_some_and(|(column, _, value)| supported(column, &[value])),
        },
        Expr::Between(b) if !b.negated => {
            matches!((column(&b.expr), literal(&b.low), literal(&b.high)),
                (Some(c), Some(low), Some(high)) if supported(c, &[low, high]))
        }
        Expr::InList(list) if !list.negated => {
            let values: Option<Vec<_>> = list.list.iter().map(literal).collect();
            matches!((column(&list.expr), values), (Some(c), Some(v)) if supported(c, &v))
        }
        Expr::IsNull(inner) | Expr::IsNotNull(inner) => column(inner).is_some(),
        _ => false,
    }
}

/// `column op literal`, with the operator flipped if the literal is on the left
fn comparison<'a>(
    left: &'a Expr,
    op: Operator,
    right: &'a Expr,
) -> Option<(&'a str, Operator, &'a ScalarValue)> {
    if !matches!(
        op,
        Operator::Eq
            | Operator::NotEq
            | Operator::Lt
            | Operator::LtEq
            | Operator::Gt
            | Operator::GtEq
    ) {
        return None;
    }
    if let (Some(c), Some(v)) = (column(left), literal(right)) {
        return Some((c, op, v));
    }
    let flipped = match op {
        Operator::Lt => Operator::Gt,
        Operator::LtEq => Operator::GtEq,
        Operator::Gt => Operator::Lt,
        Operator::GtEq => Operator::LtEq,
        other => other,
    };
    Some((column(right)?, flipped, literal(left)?))
}

/// The range for `column` and `values` as physical values, if both are known
fn leaf(ctx: &Ctx<'_>, column: &str, values: &[&ScalarValue]) -> Option<(Range, Vec<Val>)> {
    let kind = *ctx.kinds.get(column)?;
    let values = values
        .iter()
        .map(|value| to_val(value, kind))
        .collect::<Option<Vec<_>>>()?;
    Some(((ctx.range)(column)?, values))
}

/// Whether no non-null value in `range` satisfies `value op`
fn compare_empty(range: &Range, op: Operator, value: &Val) -> bool {
    let min = range.min.as_ref();
    let max = range.max.as_ref();
    match op {
        Operator::Eq => min.is_some_and(|min| value < min) || max.is_some_and(|max| value > max),
        // Bounds that meet pin every non-null value
        Operator::NotEq => min == Some(value) && max == Some(value),
        Operator::Lt => min.is_some_and(|min| min >= value),
        Operator::LtEq => min.is_some_and(|min| min > value),
        Operator::Gt => max.is_some_and(|max| max <= value),
        Operator::GtEq => max.is_some_and(|max| max < value),
        _ => false,
    }
}

fn column(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Column(column) => Some(&column.name),
        _ => None,
    }
}

fn literal(expr: &Expr) -> Option<&ScalarValue> {
    match expr {
        Expr::Literal(value, _) => Some(value),
        _ => None,
    }
}

/// A literal as the physical value of a column of `kind`
fn to_val(value: &ScalarValue, kind: Kind) -> Option<Val> {
    use ScalarValue as S;
    let int = |v: i128, scale: i8| rescale(v, 0, scale).map(Val::Int);
    match (kind, value) {
        (Kind::Int { scale }, S::Int8(Some(v))) => int(i128::from(*v), scale),
        (Kind::Int { scale }, S::Int16(Some(v))) => int(i128::from(*v), scale),
        (Kind::Int { scale }, S::Int32(Some(v)) | S::Date32(Some(v))) => int(i128::from(*v), scale),
        (Kind::Int { scale }, S::Int64(Some(v))) => int(i128::from(*v), scale),
        (Kind::Int { scale }, S::UInt8(Some(v))) => int(i128::from(*v), scale),
        (Kind::Int { scale }, S::UInt16(Some(v))) => int(i128::from(*v), scale),
        (Kind::Int { scale }, S::UInt32(Some(v))) => int(i128::from(*v), scale),
        (Kind::Int { scale }, S::UInt64(Some(v))) => int(i128::from(*v), scale),
        (Kind::Int { scale }, S::Decimal128(Some(v), _, from)) => {
            rescale(*v, *from, scale).map(Val::Int)
        }
        (Kind::Bytes, S::Utf8(Some(v)) | S::LargeUtf8(Some(v)) | S::Utf8View(Some(v))) => {
            Some(Val::Bytes(v.as_bytes().to_vec()))
        }
        (Kind::Bytes, S::Binary(Some(v)) | S::LargeBinary(Some(v)) | S::BinaryView(Some(v))) => {
            Some(Val::Bytes(v.clone()))
        }
        _ => None,
    }
}

/// `v` at scale `from` as an exact value at scale `to`, or `None` if it isn't one
fn rescale(v: i128, from: i8, to: i8) -> Option<i128> {
    let factor = |digits: i8| 10_i128.checked_pow(u32::try_from(digits).ok()?);
    match from.cmp(&to) {
        std::cmp::Ordering::Equal => Some(v),
        std::cmp::Ordering::Less => v.checked_mul(factor(to - from)?),
        std::cmp::Ordering::Greater => {
            let f = factor(from - to)?;
            (v % f == 0).then_some(v / f)
        }
    }
}
