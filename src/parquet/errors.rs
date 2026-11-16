//! Errors for the Parquet source.

use thiserror::Error;

/// Errors from opening or interpreting a Parquet file.
#[derive(Debug, Error)]
pub enum ParquetFileError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Parquet(#[from] parquet::errors::ParquetError),
    #[error("column `{column}`: unsupported {reason}")]
    UnsupportedColumn { column: String, reason: String },
}
