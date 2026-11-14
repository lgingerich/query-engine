//! Load each column as encoded Parquet pages and print one line per column.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

use parquet::errors::Result;
use parquet::file::reader::{FileReader, SerializedFileReader};

use query_engine::types::Column;

fn main() -> Result<()> {
    
    let path = Path::new("./tpch-data/region.parquet");
    let reader = SerializedFileReader::new(File::open(&path)?)?;
    let row_groups = reader.num_row_groups();
    let rows = reader.metadata().file_metadata().num_rows();
    writeln!(io::stdout(), "path={:?} row_groups={} rows={}", path, row_groups, rows)?;

    for group_index in 0..row_groups {
        let row_group = reader.get_row_group(group_index)?;
        let group_rows = row_group.metadata().num_rows();
        for column_index in 0..row_group.num_columns() {
            let descr = row_group.metadata().schema_descr().column(column_index);
            let name = descr.name();
            let physical_type = descr.physical_type();
            let mut pages = Vec::new();
            for page in row_group.get_column_page_reader(column_index)? {
                pages.push(page?);
            }
            let column = Column::new(name, physical_type, pages);
            writeln!(
                io::stdout(),
                "row_group={group_index} rows={group_rows} {column}"
            )?;
        }
    }
    Ok(())
}
