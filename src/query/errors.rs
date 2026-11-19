//! Errors for SQL parsing

use thiserror::Error;

/// Errors from turning SQL text into a parsed query.
#[derive(Debug, Error)]
pub enum QueryError {
    /// Invalid SQL
    #[error(transparent)]
    Parse(#[from] sqlparser::parser::ParserError),
    /// Valid SQL the engine doesn't support yet
    #[error("unsupported SQL: {0}")]
    Unsupported(String),
}
