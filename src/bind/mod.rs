pub(crate) mod errors;

use crate::bind::errors::BindError;
use crate::query::{CompareOp, Literal, Projection, Query};
use crate::types::{DataType, Field, Schema};
use crate::utils::{parse_date, parse_decimal};

#[derive(Debug)]
pub(crate) struct BoundQuery {
    /// Table column indices to return, in output order. `SELECT *` is `0..n`
    pub(crate) projection: Vec<usize>,
    /// `WHERE` filters, AND-ed, with columns resolved and values typed
    pub(crate) filters: Vec<BoundFilter>,
    /// `LIMIT n`
    pub(crate) limit: Option<u64>,
    /// Fields of the result, in output order
    pub(crate) output_schema: Schema,
}

#[derive(Debug)]
pub(crate) struct BoundFilter {
    /// Index into the table schema
    pub(crate) column: usize,
    /// Comparison
    pub(crate) op: CompareOp,
    /// Literal converted to the column's type
    pub(crate) value: ScalarValue,
}

#[derive(Debug)]
pub(crate) enum ScalarValue {
    Int32(i32),
    Int64(i64),
    /// Unscaled, at the column's scale
    Decimal(i64),
    /// Days since 1970-01-01
    Date32(i32),
    Utf8(String),
}

pub(crate) fn bind(query: Query, schema: &Schema) -> Result<BoundQuery, BindError> {
    // Projection
    let projection = match query.projection {
        Projection::All => (0..schema.fields().len()).collect(),
        Projection::Columns(names) => {
            let mut indices = Vec::with_capacity(names.len());
            for name in names {
                let Some(i) = schema.index_of(&name) else {
                    return Err(BindError::ColumnNotFound(name));
                };
                indices.push(i);
            }
            indices
        }
    };

    // Filter
    let mut filters = Vec::with_capacity(query.filters.len());
    for filter in query.filters {
        let Some(column) = schema.index_of(&filter.column) else {
            return Err(BindError::ColumnNotFound(filter.column));
        };
        let field = &schema.fields()[column];
        let op = filter.op;
        let value = bind_literal(field, filter.value)?;
        filters.push(BoundFilter { column, op, value });
    }

    let output_schema = Schema::new(
        projection
            .iter()
            .map(|&i| schema.fields()[i].clone())
            .collect(),
    );

    Ok(BoundQuery {
        projection,
        filters,
        limit: query.limit,
        output_schema,
    })
}

fn bind_literal(field: &Field, literal: Literal) -> Result<ScalarValue, BindError> {
    let invalid = |value: &str| BindError::InvalidLiteral {
        column: field.name.clone(),
        data_type: field.data_type,
        value: value.to_string(),
    };

    match (field.data_type, literal) {
        (DataType::Int32, Literal::Number(s)) => s
            .parse()
            .ok()
            .map(ScalarValue::Int32)
            .ok_or_else(|| invalid(&s)),
        (DataType::Int64, Literal::Number(s)) => s
            .parse()
            .ok()
            .map(ScalarValue::Int64)
            .ok_or_else(|| invalid(&s)),
        (DataType::Decimal { scale, .. }, Literal::Number(s)) => parse_decimal(&s, scale)
            .map(ScalarValue::Decimal)
            .ok_or_else(|| invalid(&s)),
        (DataType::Date32, Literal::Date(s)) => parse_date(&s)
            .map(ScalarValue::Date32)
            .ok_or_else(|| invalid(&s)),
        (DataType::Utf8, Literal::String(s)) => Ok(ScalarValue::Utf8(s)),
        (data_type, literal) => Err(BindError::TypeMismatch {
            column: field.name.clone(),
            data_type,
            literal: format!("{literal:?}"),
        }),
    }
}
