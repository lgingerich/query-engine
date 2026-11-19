//! Runs a sample query

use std::path::Path;

use query_engine::{EngineError, ParquetSource, Session};

/*
TODO:
Parse AST -> get all columns needed in query -> check file_metadata schema -> error if columns don't exist

*/

/*
High-level architecture/flow

1. open(path) -> source: file reader + footer + schema
    parse footer and convert schema, including column-chunk min/max
    page indexes and bloom filters stay unread
2. bind(query, schema) -> ScanPlan
    convert human query into engine-readable scan request
    no real operations or io here, this is like type checking
3. plan(scan_request) -> ScanPlan
    using footer metadata, decide which row groups to keep
    a surviving equality filter may then read that column's bloom filter;
    a selective filter may read the page index for those columns
4. scan(scan_plan) -> data page stream
    open the remaining row groups, read column chunks, decode data pages
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
    println!("session={session:#?}");

    // Build and execute query
    let sql = "\
        SELECT c_custkey, c_name FROM customer \
        WHERE c_custkey <= 37500 \
        AND c_nationkey > 5 \
        LIMIT 5; \
    ";
    let results = session.execute(sql)?; // mock api method

    println!("results={results:#?}");

    Ok(())
}
