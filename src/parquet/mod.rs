//! Parquet table source

pub(crate) mod errors;
mod schema;

use std::fmt;
use std::fs::File;
use std::path::Path;

use parquet::file::metadata::ParquetMetaData;
use parquet::file::reader::FileReader;
use parquet::file::serialized_reader::SerializedFileReader;

use crate::parquet::errors::ParquetFileError;
use crate::parquet::schema::convert_schema;
use crate::types::Schema;

/*
TODO: Handle data page v1 vs v2

*/

/// An opened Parquet file: reader, footer, and engine schema
pub struct ParquetSource {
    reader: SerializedFileReader<File>,
    schema: Schema,
}

impl fmt::Debug for ParquetSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ParquetSource")
            .field("reader", &std::any::type_name_of_val(&self.reader))
            .field("schema", &self.schema)
            .finish()
    }
}

impl ParquetSource {
    /// Opens a Parquet file, reads the footer, and converts the schema
    ///
    /// # Errors
    ///
    /// Fails if the file can't be opened, isn't valid Parquet, or has an
    /// unsupported column type
    pub fn open(path: &Path) -> Result<Self, ParquetFileError> {
        // NOTE: The parquet crate reader has a moderate amount of logic baked in for how footers are read and verified.
        // When working with fetches from object storage, need to re-evaluate this.
        // Footer only. Page indexes and bloom filters are read later, for the columns a query actually filters.

        // NOTE: Do not use any filereader build options that the Parquet crate has. This is only a footer read and we can't push down into this open() method.
        let reader = SerializedFileReader::new(File::open(path)?)?;
        let schema = convert_schema(reader.metadata().file_metadata().schema_descr())?;
        Ok(Self { reader, schema })
    }

    /// Engine schema for this file
    #[must_use]
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Footer metadata: row groups, row counts, column-chunk stats
    #[must_use]
    pub(crate) fn metadata(&self) -> &ParquetMetaData {
        self.reader.metadata()
    }
}
