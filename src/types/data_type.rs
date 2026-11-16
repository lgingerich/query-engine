//! Logical types and schemas. Describe columns, hold no values

/// Logical type of a column: what values mean, not how they're stored
///
/// The full set of types the engine supports. Anything else is rejected when a
/// source is opened
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataType {
    /// Signed 32-bit integer
    Int32,
    /// Signed 64-bit integer
    Int64,
    /// Fixed-point decimal stored as an unscaled `i64`, so `precision <= 18`.
    /// `12345` with `scale: 2` is `123.45`
    Decimal {
        /// Total number of digits
        precision: u8,
        /// Digits after the decimal point
        scale: i8,
    },
    /// Days since 1970-01-01
    Date32,
    /// UTF-8 string
    Utf8,
}

/// A named, typed column
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Column name
    pub name: String,
    /// Logical type
    pub data_type: DataType,
    /// Whether the column can contain nulls
    pub nullable: bool,
}

/// Ordered list of columns, column index `i` is `fields[i]`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    fields: Vec<Field>,
}

impl Schema {
    /// Creates a schema from its fields, in column order
    #[must_use]
    pub fn new(fields: Vec<Field>) -> Self {
        Self { fields }
    }

    /// All fields, in column order
    #[must_use]
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// The field at column index `i`, if any
    #[must_use]
    pub fn field(&self, i: usize) -> Option<&Field> {
        self.fields.get(i)
    }

    /// Column index of the field named `name`, if any
    #[must_use]
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.name == name)
    }
}
