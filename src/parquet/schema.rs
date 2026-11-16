//! Parquet physical schema -> engine logical schema.

use parquet::basic::{ConvertedType, Type as PhysicalType};
use parquet::schema::types::{ColumnDescriptor, SchemaDescriptor};

use crate::parquet::errors::ParquetFileError;
use crate::types::{DataType, Field, Schema};

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
        // 16-bit ints are widened to `Int32`. Every value fits, and Parquet
        // stores them as INT32 anyway
        (
            PhysicalType::INT32,
            ConvertedType::NONE
            | ConvertedType::INT_32
            | ConvertedType::INT_16
            | ConvertedType::UINT_16,
        ) => DataType::Int32,
        (PhysicalType::INT64, ConvertedType::NONE | ConvertedType::INT_64) => DataType::Int64,
        (PhysicalType::INT32, ConvertedType::DATE) => DataType::Date32,
        // INT32/INT64 storage caps precision at 9/18 digits (checked by the parquet
        // crate), so every value fits the unscaled `i64` of `Decimal`
        (PhysicalType::INT32 | PhysicalType::INT64, ConvertedType::DECIMAL) => {
            let (p, s) = (col.type_precision(), col.type_scale());
            let (Ok(precision), Ok(scale)) = (u8::try_from(p), i8::try_from(s)) else {
                return Err(unsupported(format!("decimal({p}, {s})")));
            };
            DataType::Decimal { precision, scale }
        }
        // Unannotated BYTE_ARRAY is read as a string too, since some writers
        // leave the UTF8 annotation off. Decoding must validate the bytes
        (PhysicalType::BYTE_ARRAY, ConvertedType::UTF8 | ConvertedType::NONE) => DataType::Utf8,
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parquet::schema::parser::parse_message_type;
    use parquet::schema::types::SchemaDescriptor;

    use super::convert_schema;
    use crate::types::DataType;

    fn convert(message: &str) -> Vec<DataType> {
        let parsed = parse_message_type(message).map(Arc::new);
        let Ok(parsed) = parsed else {
            panic!("bad test schema: {parsed:?}");
        };
        let schema = convert_schema(&SchemaDescriptor::new(parsed));
        let Ok(schema) = schema else {
            panic!("conversion failed: {schema:?}");
        };
        schema.fields().iter().map(|f| f.data_type).collect()
    }

    #[test]
    fn supported_types() {
        let types = convert(
            "message m {
                required int32 a;
                required int32 b (INTEGER(32,true));
                required int64 c;
                required int64 d (INTEGER(64,true));
                required int64 e (DECIMAL(15,2));
                required int32 f (DECIMAL(9,3));
                required int32 g (DATE);
                required binary h (UTF8);
            }",
        );
        assert_eq!(
            types,
            [
                DataType::Int32,
                DataType::Int32,
                DataType::Int64,
                DataType::Int64,
                DataType::Decimal {
                    precision: 15,
                    scale: 2
                },
                DataType::Decimal {
                    precision: 9,
                    scale: 3
                },
                DataType::Date32,
                DataType::Utf8,
            ]
        );
    }

    #[test]
    fn widened_and_unannotated() {
        let types = convert(
            "message m {
                required int32 signed16 (INTEGER(16,true));
                required int32 unsigned16 (INTEGER(16,false));
                optional binary text;
            }",
        );
        assert_eq!(types, [DataType::Int32, DataType::Int32, DataType::Utf8]);
    }

    #[test]
    fn other_types_rejected() {
        for column in [
            "required int32 u (INTEGER(32,false));",
            "required int32 tiny (INTEGER(8,true));",
            "required double d;",
            "required boolean b;",
        ] {
            let parsed = parse_message_type(&format!("message m {{ {column} }}"));
            let Ok(parsed) = parsed else {
                panic!("bad test schema: {parsed:?}");
            };
            assert!(
                matches!(
                    convert_schema(&SchemaDescriptor::new(Arc::new(parsed))),
                    Err(crate::parquet::errors::ParquetFileError::UnsupportedColumn { .. })
                ),
                "{column}"
            );
        }
    }
}
