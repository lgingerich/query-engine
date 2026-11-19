# Benchmark queries

One query per file, named by file stem. Copied from Apache DataFusion 55.2.0,
`benchmarks/queries/`, under the Apache License 2.0: see `LICENSE` and
`NOTICE` in this directory. The rest of the repository is MIT.

- `clickbench/`: the 43 ClickBench queries. `q00` is ClickBench Q0. The
  DataFusion setup comments are removed; `benches/common/mod.rs` sets
  `binary_as_string` instead.
- `setup/clickbench-datafusion.sql`: from ClickBench's
  `datafusion-partitioned/create.sql`. A view that gives `EventDate` a date
  type, which q36-q42 need.
- `setup/clickbench-duckdb.sql`: written here, the same view in DuckDB's
  SQL, for the bounds cross-checks.
- `tpch/`: the 22 TPC-H queries, unchanged.

Identifiers are double-quoted where the column names are mixed case, since
DataFusion lowercases unquoted identifiers.
