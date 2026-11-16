//! Top-level error type for the query engine.

use thiserror::Error;

use crate::parquet::errors::ParquetFileError;
use crate::query::errors::QueryError;

/// Any error the engine can return.
#[derive(Debug, Error)]
pub enum EngineError {
    /// Reading a Parquet file failed.
    #[error(transparent)]
    Parquet(#[from] ParquetFileError),
    /// SQL couldn't be parsed or isn't supported.
    #[error(transparent)]
    Query(#[from] QueryError),
    /// I/O outside a specific source, e.g. writing results.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// `Result` with [`EngineError`] as the default error type.
pub type Result<T, E = EngineError> = std::result::Result<T, E>;
