//! Runs a sample query

use std::path::Path;

use query_engine::errors::EngineError;
use query_engine::parquet::ParquetSource;
use query_engine::session::Session;

/*
TODO:
Parse AST -> get all columns needed in query -> check file_metadata schema -> error if columns don't exist

*/

/*
High-level architecture/flow

1. open(path) -> source: file reader + footer + schema
    parse footer and convert schema (not reading any real file data here yet)
2. bind(query, schema) -> ScanPlan
    convert human query into engine-readable scan request
    no real operations or io here, this is like type checking
3. plan(scan_request) -> ScanPlan
    using Parquet metadata, plan out what row groups and data we keep
    no io or execution here, only checks against metadata
4. scan(scan_plan) -> data page stream
    open row groups, read column chunks, decode data pages
    real data io is happening here now
    output stream/iterator of data
5. execute
    start executing the query
    filtering + aggregation + etc.


Various Notes
- Projection = column pruning
- Filtering = row pruning
*/

fn main() -> Result<(), EngineError> {
    // Register table
    let path = Path::new("./tpch-data/customer.parquet");
    let mut session = Session::new();
    session.register("customer", ParquetSource::open(path)?)?;

    // Build and execute query
    let sql = "\
        SELECT * FROM customer \
        WHERE c_custkey <= 37500 \
        LIMIT 5; \
    ";
    let results = session.execute(sql)?; // mock api method

    println!("{results:#?}");

    Ok(())
}
