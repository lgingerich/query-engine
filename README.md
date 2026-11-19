# query-engine

A columnar/analytical query engine written from scratch. The goal is to learn and implement the big pieces of what make up a modern query engine, not to build the latest algorithms for micro-optimizations in query execution. Query performance benchmarks are run against Clickbench and TPC-H.

The key pieces are:

- **Reads:** Skip, decompress, and decode only the bytes the query needs. Parquet files are the current target.
- **Execution:** Keep hardware saturated, efficiently pipeline/parallelize work, auto-vectorization, push vs. pull model, etc.
- **Optimizer:** Build a query optimizer. This is what turns a query into the plan that the execution stage runs.

## Benchmarking

`scripts/perf` runs everything: hardware limits, query timings with allocation counts, CPU profiles, and heap profiles, with DataFusion as a reference engine. See [docs/BENCHMARKING.md](./docs/BENCHMARKING.md) for what each layer is for and when to use it.

## License

MIT. See [LICENSE](./LICENSE) for the full text.
