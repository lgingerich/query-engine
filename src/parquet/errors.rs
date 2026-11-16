//! Errors for the Parquet source.

use thiserror::Error;

/// Errors from opening or interpreting a Parquet file.
#[derive(Debug, Error)]
pub enum ParquetFileError {
    /// File couldn't be opened or read
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Parquet crate rejected the file (bad footer, corrupt metadata, etc.)
    #[error(transparent)]
    Parquet(#[from] parquet::errors::ParquetError),
    /// Column type or layout the engine doesn't support yet
    #[error("column `{column}`: unsupported {reason}")]
    UnsupportedColumn {
        /// Column path
        column: String,
        /// What isn't supported
        reason: String,
    },
}
