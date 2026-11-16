//! Parquet physical schema -> engine logical schema.

use parquet::basic::{ConvertedType, Type as PhysicalType};
use parquet::schema::types::{ColumnDescriptor, SchemaDescriptor};

use crate::parquet::errors::ParquetFileError;
use crate::schema::{DataType, Field, Schema};

/// Converts a Parquet schema to an engine schema.
///
/// Invariant relied on by planning and decoding: engine column `i` is Parquet
/// leaf column `i`. This holds because nested columns are rejected, so every
/// leaf is a top-level column.
pub(crate) fn convert_schema(descr: &SchemaDescriptor) -> Result<Schema, ParquetFileError> {
    let fields = descr
        .columns()
        .iter()
        .map(|col| convert_column(col))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Schema::new(fields))
}

fn convert_column(col: &ColumnDescriptor) -> Result<Field, ParquetFileError> {
    let unsupported = |reason: String| ParquetFileError::UnsupportedColumn {
        column: col.path().string(),
        reason,
    };

    if col.path().parts().len() > 1 || col.max_rep_level() > 0 {
        return Err(unsupported("nested or repeated column".to_string()));
    }

    // parquet-rs fills `converted_type` from the logical type, so the match
    // below covers old and new writers. Logical types with no converted-type
    // equivalent (nanosecond TIME/TIMESTAMP, UUID, FLOAT16, VARIANT, geo, ...)
    // come through as `NONE` and would be misread as the bare physical type.
    if col.converted_type() == ConvertedType::NONE
        && let Some(logical) = col.logical_type_ref()
    {
        return Err(unsupported(format!("logical type {logical:?}")));
    }

    let data_type = match (col.physical_type(), col.converted_type()) {
        (PhysicalType::BOOLEAN, ConvertedType::NONE) => DataType::Boolean,
        (PhysicalType::INT32, ConvertedType::NONE | ConvertedType::INT_32) => DataType::Int32,
        (PhysicalType::INT64, ConvertedType::NONE | ConvertedType::INT_64) => DataType::Int64,
        (PhysicalType::FLOAT, ConvertedType::NONE) => DataType::Float32,
        (PhysicalType::DOUBLE, ConvertedType::NONE) => DataType::Float64,
        (PhysicalType::INT32, ConvertedType::DATE) => DataType::Date32,
        (PhysicalType::INT32 | PhysicalType::INT64, ConvertedType::DECIMAL) => {
            let (p, s) = (col.type_precision(), col.type_scale());
            let (Ok(precision), Ok(scale)) = (u8::try_from(p), i8::try_from(s)) else {
                return Err(unsupported(format!("decimal({p}, {s})")));
            };
            DataType::Decimal { precision, scale }
        }
        (
            PhysicalType::BYTE_ARRAY,
            ConvertedType::UTF8 | ConvertedType::ENUM | ConvertedType::JSON,
        ) => DataType::Utf8,
        (PhysicalType::BYTE_ARRAY, ConvertedType::NONE) => DataType::Binary,
        (physical, converted) => {
            return Err(unsupported(format!("type {physical} / {converted}")));
        }
    };

    Ok(Field {
        name: col.name().to_string(),
        data_type,
        nullable: col.self_type().is_optional(),
    })
}
