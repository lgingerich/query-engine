//! Top-level error type for the query engine.

use thiserror::Error;

// use crate::filters::BindError;
use crate::parquet::errors::ParquetFileError;

/// Any error the engine can return.
#[derive(Debug, Error)]
pub enum EngineError {
    /// Reading a Parquet file failed.
    #[error(transparent)]
    Parquet(#[from] ParquetFileError),
    // /// A filter didn't match the table's schema.
    // #[error(transparent)]
    // Bind(#[from] BindError),
    /// I/O outside a specific source, e.g. writing results.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// `Result` with [`EngineError`] as the default error type.
pub type Result<T, E = EngineError> = std::result::Result<T, E>;
