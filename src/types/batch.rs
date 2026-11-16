//! A group of rows, stored by column

use thiserror::Error;

use super::column::Column;

/// Why a batch couldn't be built
#[derive(Debug, Error)]
pub enum BatchError {
    /// A column's row count doesn't match the batch
    #[error("column {index} has {actual} rows, batch has {expected}")]
    RowCount {
        /// Column position
        index: usize,
        /// Rows in the batch
        expected: usize,
        /// Rows in the column
        actual: usize,
    },
}

/// Rows stored by column. Every column has `num_rows` rows
#[derive(Debug)]
pub struct Batch {
    num_rows: usize,
    columns: Vec<Column>,
}

impl Batch {
    /// Builds a batch, checking every column has `num_rows` rows
    ///
    /// # Errors
    ///
    /// Fails if a column has the wrong number of rows
    pub fn try_new(num_rows: usize, columns: Vec<Column>) -> Result<Self, BatchError> {
        for (index, column) in columns.iter().enumerate() {
            if column.len() != num_rows {
                return Err(BatchError::RowCount {
                    index,
                    expected: num_rows,
                    actual: column.len(),
                });
            }
        }
        Ok(Self { num_rows, columns })
    }

    /// Number of rows
    #[must_use]
    pub fn num_rows(&self) -> usize {
        self.num_rows
    }

    /// Columns, in schema order
    #[must_use]
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }
}
