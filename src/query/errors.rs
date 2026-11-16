//! Errors for SQL parsing

use thiserror::Error;

/// Errors from turning SQL into a [`Query`](crate::query::Query)
#[derive(Debug, Error)]
pub enum QueryError {
    /// Invalid SQL
    #[error(transparent)]
    Parse(#[from] sqlparser::parser::ParserError),
    /// Valid SQL the engine doesn't support yet
    #[error("unsupported SQL: {0}")]
    Unsupported(String),
}
