/// Logical type of a column: what values mean, not how a file stores them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    /// `true` / `false`.
    Boolean,
    /// Signed 32-bit integer.
    Int32,
    /// Signed 64-bit integer.
    Int64,
    /// 32-bit IEEE float.
    Float32,
    /// 64-bit IEEE float.
    Float64,
    /// Fixed-point decimal: `precision` total digits, `scale` after the point.
    Decimal {
        /// Total number of digits.
        precision: u8,
        /// Digits after the decimal point.
        scale: i8,
    },
    /// Days since 1970-01-01.
    Date32,
    /// UTF-8 string.
    Utf8,
    /// Raw bytes.
    Binary,
}

#[derive(Debug)]
pub struct Field {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
}

#[derive(Debug)]
pub struct Schema {
    fields: Vec<Field>,
}

impl Schema {
    /// Creates a schema from its fields, in column order.
    #[must_use]
    pub fn new(fields: Vec<Field>) -> Self {
        Self { fields }
    }

    /// All fields, in column order.
    #[must_use]
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// The field at column index `i`, if any.
    #[must_use]
    pub fn field(&self, i: usize) -> Option<&Field> {
        self.fields.get(i)
    }

    /// Column index of the field named `name`, if any.
    #[must_use]
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.name == name)
    }
}
