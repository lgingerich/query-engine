//! Load each column as encoded Parquet pages and print one line per column.

use std::fs::File;
use std::path::Path;

use parquet::errors::Result;
use parquet::file::reader::{FileReader, SerializedFileReader};

use parquet::file::serialized_reader::ReadOptionsBuilder;
use parquet::file::statistics::Statistics;
use query_engine::filters::{Filter, Operations};

/*
TODO:
Parse AST -> get all columns needed in query -> check file_metadata schema -> error if columns don't exist

*/

fn main() -> Result<()> {
    // Representative query for customer.parquet:
    // SELECT * FROM customer WHERE c_custkey <= 37500;
    let filter = Filter {
        col: "c_custkey".to_string(),
        op: Operations::LTE,
        val: 37_500,
    };

    let path = Path::new("./tpch-data/customer.parquet");
    let read_options = ReadOptionsBuilder::new().build();
    let reader = SerializedFileReader::new_with_options(File::open(path)?, read_options)?;

    // These are the 3 levels/types of metadata that exist in the Parquet footer
    let file_metadata = reader.metadata().file_metadata();
    let row_groups = reader.metadata().row_groups();
    let _page_index = reader.metadata().page_index();

    let schema = file_metadata.schema_descr();
    let column_index = schema
        .columns()
        .iter()
        .position(|column| column.name() == filter.col)
        .expect("column missing");

    let mut rg_keep: Vec<u32> = Vec::new(); // empty vec to keep valid row group indexes in
    let mut i: u32 = 0; // temp way to get index since I'm iterating directly over row_groups

    for rg in row_groups {
        let col = rg.column(column_index);

        let Some(Statistics::Int64(stats)) = col.statistics() else {
            // Keep row group if no stats exist
            rg_keep.push(i);
            i += 1;
            continue;
        };

        let min = if stats.min_is_exact() {
            stats.min_opt().copied()
        } else {
            None
        };
        let max = if stats.max_is_exact() {
            stats.max_opt().copied()
        } else {
            None
        };

        let none_match = match filter.op {
            Operations::GT => max.is_some_and(|max| max <= filter.val),
            Operations::GTE => max.is_some_and(|max| max < filter.val),
            Operations::LT => min.is_some_and(|min| min >= filter.val),
            Operations::LTE => min.is_some_and(|min| min > filter.val),
        };

        if none_match {
            i += 1;
            continue;
        }
        rg_keep.push(i);
        i += 1;
    }

    println!("rg_keep={rg_keep:#?}");

    // let file_schema = file_metadata.schema();
    // let rows = file_metadata.num_rows();

    // let schema_basic_info = file_schema.get_basic_info();
    // let schema_name = file_schema.name();
    // let schema_fields = file_schema.get_fields();
    // let schema_physical_type = file_schema.get_physical_type();
    // let schema_precision = file_schema.get_precision();
    // let schema_scale = file_schema.get_scale();

    // writeln!(
    //     io::stdout(),
    //     "schema={file_schema:?}",
    // )?;
    // writeln!(
    //     io::stdout(),
    //     "path={} row_groups={num_row_groups} rows={rows}",
    //     path.display(),
    // )?;

    // for group_index in 0..num_row_groups {
    //     let row_group = reader.get_row_group(group_index)?;
    //     let group_rows = row_group.metadata().num_rows();
    //     for column_index in 0..row_group.num_columns() {
    //         let descr = row_group.metadata().schema_descr().column(column_index);
    //         let name = descr.name();
    //         let physical_type = descr.physical_type();
    //         let mut pages = Vec::new();
    //         for page in row_group.get_column_page_reader(column_index)? {
    //             pages.push(page?);
    //         }
    //         let column = Column::new(name, physical_type, pages);
    //         writeln!(
    //             io::stdout(),
    //             "row_group={group_index} rows={group_rows} {column}"
    //         )?;
    //     }
    // }
    Ok(())
}
