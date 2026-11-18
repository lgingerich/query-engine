//! Parquet table source

pub(crate) mod errors;
mod schema;

use std::fmt;
use std::fs::File;
use std::path::Path;

use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::file::serialized_reader::ReadOptionsBuilder;
use parquet::schema::types::SchemaDescriptor;

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
        let read_options = ReadOptionsBuilder::new().build();
        let reader = SerializedFileReader::new_with_options(File::open(path)?, read_options)?;
        let schema = convert_schema(reader.metadata().file_metadata().schema_descr())?;
        Ok(Self { reader, schema })
    }

    /// Engine schema for this file
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    fn parquet_schema(&self) -> &SchemaDescriptor {
        self.reader.metadata().file_metadata().schema_descr()
    }
}

// // These are the 3 levels/types of metadata that exist in the Parquet footer
// let file_metadata = reader.metadata().file_metadata();
// let row_groups = reader.metadata().row_groups();
// let _page_index = reader.metadata().page_index();

// let schema = file_metadata.schema_descr();

// let rgs = row_groups_to_keep(row_groups, schema, &filter);

// pub fn row_groups_to_keep(
//     row_groups: &[RowGroupMetaData],
//     schema: &SchemaDescriptor,
//     filter: &Filter,
// ) -> Vec<usize> {

//     let column_index = schema
//         .columns()
//         .iter()
//         .position(|column| column.name() == filter.col_name)
//         .expect("column missing");

//     let column_type = schema.column(column_index).physical_type();
//     let mut rg_keep: Vec<usize> = Vec::new(); // empty vec to keep valid row group indexes in
//     let mut i: usize = 0; // temp way to get index since I'm iterating directly over row_groups

//     for rg in row_groups {
//         let col = rg.column(column_index);

//         let Some(Statistics::Int64(stats)) = col.statistics() else {
//             // Keep row group if no stats exist
//             rg_keep.push(i);
//             i += 1;
//             continue;
//         };

//         let min = if stats.min_is_exact() {
//             stats.min_opt().copied()
//         } else {
//             None
//         };
//         let max = if stats.max_is_exact() {
//             stats.max_opt().copied()
//         } else {
//             None
//         };

//         let none_match = match filter.op {
//             Operations::GT => max.is_some_and(|max| max <= filter.val),
//             Operations::GTE => max.is_some_and(|max| max < filter.val),
//             Operations::LT => min.is_some_and(|min| min >= filter.val),
//             Operations::LTE => min.is_some_and(|min| min > filter.val),
//         };

//         if none_match {
//             i += 1;
//             continue;
//         }
//         rg_keep.push(i);
//         i += 1;
//     }
//     rg_keep
// }
