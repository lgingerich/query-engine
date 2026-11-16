//! One column's values in memory

use super::bitmap::Bitmap;
use super::data_type::DataType;

/// Values for one column. One variant per in-memory layout, not per `DataType`
#[derive(Debug)]
pub enum ColumnData {
    /// `Int32`, `Date32`
    I32(Vec<i32>),
    /// `Int64`, `Decimal` (unscaled)
    I64(Vec<i64>),
    /// `Utf8`
    Str(Vec<String>),
}

impl ColumnData {
    /// Empty storage for `data_type`
    ///
    /// The one place logical types map to storage: `Date32` is stored as `I32`,
    /// `Decimal` as unscaled `I64`
    #[must_use]
    pub fn with_capacity(data_type: DataType, rows: usize) -> Self {
        match data_type {
            DataType::Int32 | DataType::Date32 => Self::I32(Vec::with_capacity(rows)),
            DataType::Int64 | DataType::Decimal { .. } => Self::I64(Vec::with_capacity(rows)),
            DataType::Utf8 => Self::Str(Vec::with_capacity(rows)),
        }
    }

    /// Number of values
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::I32(values) => values.len(),
            Self::I64(values) => values.len(),
            Self::Str(values) => values.len(),
        }
    }

    /// Whether there are no values
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One column's values, plus which rows are null
#[derive(Debug)]
pub struct Column {
    data: ColumnData,
    /// `None` = no nulls
    validity: Option<Bitmap>,
}

impl Column {
    /// Column with no nulls
    #[must_use]
    pub fn new(data: ColumnData) -> Self {
        Self {
            data,
            validity: None,
        }
    }

    /// The values
    #[must_use]
    pub fn data(&self) -> &ColumnData {
        &self.data
    }

    /// Null bitmap, if the column has nulls
    #[must_use]
    pub fn validity(&self) -> Option<&Bitmap> {
        self.validity.as_ref()
    }

    /// Number of rows
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether there are no rows
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{ColumnData, DataType};

    #[test]
    fn storage_for_supported_types() {
        let decimal = DataType::Decimal {
            precision: 15,
            scale: 2,
        };
        assert!(matches!(
            ColumnData::with_capacity(DataType::Int32, 4),
            ColumnData::I32(_)
        ));
        assert!(matches!(
            ColumnData::with_capacity(DataType::Date32, 4),
            ColumnData::I32(_)
        ));
        assert!(matches!(
            ColumnData::with_capacity(DataType::Int64, 4),
            ColumnData::I64(_)
        ));
        assert!(matches!(
            ColumnData::with_capacity(decimal, 4),
            ColumnData::I64(_)
        ));
        assert!(matches!(
            ColumnData::with_capacity(DataType::Utf8, 4),
            ColumnData::Str(_)
        ));
    }
}
