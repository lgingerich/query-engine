//! Engine-owned schema and in-memory data

// Batch { rows: 3, cols: [Column, Column] }
//
//   Column 0                     Column 1
//   data: I64     validity       data: Str     validity
//   ┌──────┐      ┌───────┐      ┌───────┐
//   │  10  │      │ true  │      │ "foo" │     None
//   │   0  │      │ false │      │ "bar" │     (no nulls)
//   │  30  │      │ true  │      │ "baz" │
//   └──────┘      └───────┘      └───────┘
//
// Row i is entry i of every column.
// A null still occupies a slot in `data`; validity[i] == false marks it.

mod batch;
mod bitmap;
mod column;
mod data_type;

pub use batch::{Batch, BatchError};
pub use bitmap::Bitmap;
pub use column::{Column, ColumnData};
pub use data_type::{DataType, Field, Schema};
