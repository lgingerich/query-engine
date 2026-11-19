# Architecture

Keep this file short (~1 page). Project-specific invariants live here; universal Rust workflow lives in `AGENTS.md`.

## Purpose
`query-engine` is a columnar SQL query engine written from scratch as a learning project, targeting Parquet files and benchmarked on ClickBench and TPC-H. Its goals are reads that skip and decode only needed bytes, efficient parallel execution, and an optimizer. At the committed HEAD only the front half exists: `Session::execute` parses and binds a query, then returns an empty `QueryResult`; planning, scanning and execution are unwritten (`src/session.rs` steps 3 and 4 are empty). The design intent is in `docs/IDEAS.md`, the Parquet layout in `docs/PARQUET.md`, and measurement in `docs/BENCHMARKING.md`; this file does not repeat them.

## Module Boundaries
All modules are private in `src/lib.rs`; the public API is the re-exports there (`Session`, `QueryResult`, `ParquetSource`, `Batch`, `Column`, `Schema`, the error types).
- `src/types/`: engine-owned schema and in-memory data (`DataType`, `Field`, `Schema`, `Column`, `Bitmap`, `Batch`). Depends on no other engine module.
- `src/parquet/`: `ParquetSource` (open file, read footer) and `schema.rs`, which converts Parquet schemas to engine `Schema`. Depends on `types`.
- `src/query/`: SQL text to the engine `Query` (`parse`). Depends on nothing else in the crate.
- `src/bind/`: resolves a `Query` against a `Schema` into a `BoundQuery` (column indices, typed `ScalarValue`s). Depends on `query`, `types`, `utils`.
- `src/session.rs`: `Session` registers tables and runs the pipeline parse -> bind -> (plan, scan, execute not yet implemented). Depends on all of the above.
- `src/errors.rs`: `EngineError`, wrapping the per-module errors (`ParquetFileError`, `QueryError`, `BindError`) via `#[from]`.
- `src/utils.rs`: literal parsers (`parse_decimal`, `parse_date`).
- `benches/` and `tests/bounds/`: benchmarks, with DataFusion and DuckDB as references, driven by `scripts/perf`. `tests/bounds` compiles the `benches/bounds/*.rs` modules via `#[path]` to test them.
- `src/lib.rs` also declares `mod plan;`, but `src/plan/` is not in the committed tree, so HEAD does not compile as committed.

## Core Invariants
- `src/query/` is the only module that touches `sqlparser`; the rest of the engine sees only its own `Query`, `Projection`, `CompareOp` and `Literal` types.
- Literals stay unparsed text in `Query`; only `bind` converts them, because only bind knows the column type. `bind` fixes column names to indices and values to the column type.
- Engine column `i` is Parquet leaf column `i`. This holds because `convert_schema` rejects nested and repeated columns, and planning and decoding rely on it.
- Null values still occupy a slot in `Column` data; the validity `Bitmap` marks them, and row `i` is entry `i` of every column (`src/types/mod.rs`).
- WHERE filters are AND-ed only (`BoundQuery.filters`).
- Each module owns its error type; `EngineError` is the only crate-wide error.

## Forbidden Patterns / Dependencies
- `sqlparser` types must not leak outside `src/query/`; `parquet` crate types must not leak outside `src/parquet/`.
- `datafusion`, `dhat`, `futures` and `tokio` are dev-dependencies for benches and tests only; do not use them in `src/`.
- `unsafe_code` is denied in `Cargo.toml`.
