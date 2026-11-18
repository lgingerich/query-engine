/// Errors from resolving a [`Query`](crate::query::Query) against a table

use thiserror::Error;
use crate::types::DataType;

#[derive(Debug, Error)]
pub enum BindError {
    /// Column name not in the table schema
    #[error("column `{0}` not found")]
    ColumnNotFound(String),
    /// Literal kind can't be compared with the column's type, e.g. a string
    /// against an `Int64` column
    #[error("column `{column}` is {data_type:?}, can't compare with {literal}")]
    TypeMismatch {
        /// Column name
        column: String,
        /// Column type
        data_type: DataType,
        /// Literal kind as written, e.g. "string `'x'`"
        literal: String,
    },
    /// Literal has the right kind but doesn't fit the column type: overflow,
    /// a bad date, or more fractional digits than the decimal scale
    #[error("column `{column}`: `{value}` is not a valid {data_type:?}")]
    InvalidLiteral {
        /// Column name
        column: String,
        /// Column type
        data_type: DataType,
        /// Literal text
        value: String,
    },
}