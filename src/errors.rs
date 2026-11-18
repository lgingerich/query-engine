//! Top-level error type for the query engine.

use thiserror::Error;

pub use crate::bind::errors::BindError;
pub use crate::parquet::errors::ParquetFileError;
pub use crate::query::errors::QueryError;

/// Any error the engine can return.
#[derive(Debug, Error)]
pub enum EngineError {
    /// Reading a Parquet file failed.
    #[error(transparent)]
    Parquet(#[from] ParquetFileError),
    /// SQL couldn't be parsed or isn't supported.
    #[error(transparent)]
    Query(#[from] QueryError),
    /// Query doesn't match the table: unknown column or type error
    #[error(transparent)]
    Bind(#[from] BindError),
    /// No table registered under this name
    #[error("table `{0}` not found")]
    TableNotFound(String),
    /// A table is already registered under this name
    #[error("table `{0}` already exists")]
    TableExists(String),
    /// I/O outside a specific source, e.g. writing results.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// `Result` with [`EngineError`] as the default error type.
pub type Result<T, E = EngineError> = std::result::Result<T, E>;
