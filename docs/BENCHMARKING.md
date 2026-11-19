# Benchmarking

Everything runs through one script, `scripts/perf`. There are four layers, each answering a different question, plus the bounds: a static reference, computed rarely and committed, that the measurements are compared against. Start at the top and go down a layer only when the one above tells you where to look.

| Layer | Question | Command | Output |
|---|---|---|---|
| 0. Hardware limits | How fast could anything go on this machine? | `scripts/perf limits` | Table: file read and memory bandwidth in GB/s |
| Reference: bounds | What is the least work each query needs on these files? | `scripts/perf bounds <suite>` | Committed table per dataset: bytes, rows, pages |
| 1. Query timing | How fast is each query, and did my change help? | `scripts/perf run <suite>`, `scripts/perf compare` | Table: time, rows, allocations, memory per query |
| 2. CPU profile | Where does a query spend its time? | `scripts/perf profile <suite> <query>` | Flamegraph in the browser (samply) |
| 3. Heap profile | Which code allocates, how much, and how often? | `scripts/perf heap <suite> <query>` | Per-call-site tree in the browser (dhat) |

`scripts/perf help` lists every command and setting.

`run` and `bounds` start a fresh process for each query, so no query sees another's caches, allocator state, or metrics. The OS page cache stays warm on purpose: timings come after a warmup run, and the bounds count bytes, not time.

## Setup

**Data** is gitignored, so fetch it once:

```bash
# TPC-H, scale factor 1: one Parquet file per table in tpch-data/
tpchgen-cli --scale-factor 1 --format parquet --parquet-compression='ZSTD(1)' --output-dir tpch-data

# ClickBench, suite clickbench: the first partition (1M rows, 122 MB)
mkdir -p clickbench-data/partitioned
curl -o clickbench-data/partitioned/hits_0.parquet \
  https://datasets.clickhouse.com/hits_compatible/athena_partitioned/hits_0.parquet

# ClickBench, suite clickbench-full: the official single file (100M rows, 14.8 GB)
curl -o clickbench-data/hits.parquet https://datasets.clickhouse.com/hits_compatible/hits.parquet
```

These match what the standard benchmarks use:

- **ClickBench:** the official files, which every engine downloads. They have no page index, and `hits_0` has 2 row groups.
- **TPC-H:** there is no official Parquet version, so this follows DataFusion's benchmark script (`benchmarks/bench.sh` at 55.2.0): `tpchgen-cli` with `ZSTD(1)`. Its `--parts=1` writes the same bytes into one directory per table; leaving it out gives one file per table, which this engine opens directly. These files have a page index (column and offset index on every column chunk), and `lineitem` has 53 row groups of about 113k rows.

The full ClickBench data set is one 14.8 GB file (`hits.parquet`, 99,997,497 rows, 226 row groups) or the same rows in 100 partitions. Both suites run the same 43 queries: `clickbench` on the first partition, for quick iteration, and `clickbench-full` on the single file, for real data sizes. A `run` of `clickbench-full` takes minutes, not seconds. `docs/PARQUET.md` covers what these files contain.

**Tools:** `jq` and `column` (built into macOS), `samply` for layer 2 (`cargo install samply`), and the `duckdb` CLI for the bounds, which refuse to run without its cross-checks. dhat is a Rust crate and builds with the benches.

**Builds:** the first `scripts/perf` command compiles every dependency, DataFusion included: about 7 minutes. After that, a cached build is instant, and a change to engine or bench code relinks in about 4 minutes with the default profile (see [Settings](#settings)).

## Layer 0: hardware limits

```bash
scripts/perf limits            # uses the ClickBench file
scripts/perf limits <file>     # any file, for the read measure
```

| Measure | What it is |
|---|---|
| `read_file` | Read the file sequentially. With the file in the OS page cache (normal after any run), this is page-cache speed, not disk. For the disk, run `sudo purge` first. |
| `sum_1thread` | Sum 1 GiB of `u64` on one core: memory bandwidth for one thread |
| `sum_all_threads` | The same, split across every core: total memory bandwidth |

**When:** once per machine, or after an OS update.

**This machine:** Apple M3 Max, 14 cores (10 performance, 4 efficiency), 36 GiB RAM, macOS 26.6.2. Measured 2026-11-13, plugged in, median of 5:

| Limit | Result |
|---|---|
| Read `hits_0.parquet` from the page cache | 16.9 GB/s |
| Memory bandwidth, 1 thread | 83.8 GB/s |
| Memory bandwidth, 14 threads | 123 GB/s |

One thread already reaches about two thirds of the machine's memory bandwidth. A cold read from disk hasn't been measured. In Low Power Mode the same run gave 37 and 49 GB/s, which is why the script warns about it.

**How to use:** divide a query's data volume by its time and compare with these numbers. For example, TPC-H Q6 on DataFusion reads four `lineitem` columns: 298 MiB decoded in 16 ms (2026-11-19, median of 10), about 19 GB/s. The machine moves 123 GB/s, so Q6 is limited by CPU work (decoding and filtering), not memory, and making that work cheaper is what will help. A query running close to a limit can only get faster by moving fewer bytes.

## Bounds: the least work a query needs

```bash
scripts/perf bounds tpch             # all 22 queries: rewrites benches/reference/bounds/tpch-sf1.tsv
scripts/perf bounds clickbench-full  # all 43: rewrites benches/reference/bounds/clickbench-full.tsv (about 2 minutes)
scripts/perf bounds tpch q06         # one query: saved in bench-results/bounds/ instead
scripts/perf bounds clickbench       # the 1M-row suite: also bench-results/ only
```

For each query and table scan: the least work any engine could do on these exact files. Committed bounds exist for the two reference datasets, TPC-H SF1 and full ClickBench; the 1M-row `clickbench` suite is for fast timing iterations, and with only 2 row groups its bounds aren't representative. A full run rewrites the committed file, and prints a `git diff --stat`, so a changed bound shows up in review. The file's `#` header records the data: generator command, file sizes, and SHA-256 prefixes. Bounds describe those exact files; regenerating the data at another scale factor writes a new file (`tpch-sf10.tsv`).

**When:** rarely: after changing the data, the queries, or the tool. The committed files are the reference that timings and your engine's actual bytes and values are compared against, so a run favors being exactly right over being quick.

### Kinds of number

- **Reachable:** the best any engine can do with what the file says (footer, statistics, page index). Hit it exactly.
- **Oracle:** what a reader that already knew the matching rows would touch. Not reachable; the gap from reachable is what a different file layout would gain.
- **Model:** time floors from bytes and the layer 0 limits. Not computed yet.

The levels, from loose to tight:

| Level | Columns | What it reads |
|---|---|---|
| Projection | `proj_bytes`, `proj_uncomp` | every column chunk of the needed columns, read whole: no statistics used |
| Reachable | `reach_bytes`, `reach_uncomp`, `reach_values` | row groups and pages that statistics can't rule out, except those whose every value statistics give |
| Late materialization (reachable) | `late_bytes`, `late_values` | filter columns as reachable; other columns only pages (or, without a page index, row groups) holding a match. Reachable by reading the filter columns first |
| Oracle | `oracle_bytes`, `oracle_values` | every needed column only where it holds a match |

### How it works

1. **Plan:** DataFusion's optimized plan gives each table scan's columns and the filters pushed into it. Joins, subqueries and views need no special handling. Each table is pruned by its own filters only; rows ruled out by joins (only `lineitem` rows whose order survives) are not counted.
2. **Metadata:** footer, column chunk sizes and statistics, and the page index where the file has one (TPC-H yes, ClickBench no). A chunk's dictionary page counts whenever any of its data pages does. Statistics are treated as bounds, as Parquet allows, never as values that occur. A chunk or page whose bounds meet with no nulls, or that is all null, has every value given by the metadata, so below projection it costs nothing: TPC-H's `o_shippriority` is 0 in every row, for example. DataFusion likewise turns such a column into a constant.
3. **Pruning, from first principles** (`benches/bounds/stats.rs`): `column op literal`, `BETWEEN`, `IN`, `IS [NOT] NULL`, under `AND` and `OR`, on integer, date, decimal and string columns. Rows are split at every page boundary of the filter columns, and a segment is dropped only if a filter can't be true anywhere in it. Anything else is kept, and counted in `unusable`.
4. **Data pass:** reads only the filter columns and evaluates every pushed filter with DataFusion's expressions (so `LIKE` and functions count), recording each matching row.
5. **Checks**, built in:
   - Any row group or page ruled out must have no matching row, or the tool stops: a bound must never be too good.
   - `df_rg_pruned`, `df_pages_pruned`, `df_bytes`: what DataFusion prunes and reads for each scan run on its own: the scan's columns, with its pushed filters. Inside the whole query DataFusion can also prune with filters it builds at run time (from a join's other side, or a top-k), and two scans of one file can't be told apart in its metrics; on its own, a scan is exactly the static problem the bounds solve. The `TOTAL` row sums the scans. A warning is raised if DataFusion prunes more row groups than the bounds, or reads fewer bytes than the reachable bound: either means the pruning logic misses a case. DataFusion counts pages differently, so compare row groups and bytes.
   - `duckdb`: DuckDB counts each scan's matching rows, and on the `TOTAL` row the query's output rows, as `SELECT count(*)` over each query statement. ClickBench gets the same `hits` view in DuckDB's SQL (`benches/queries/setup/clickbench-duckdb.sql`). `ok`, `diff:<DuckDB's number>`, or `n/a` when DuckDB can't run the SQL, such as a ClickBench query using DataFusion-only functions.
   - Warnings print as they happen and are also written into the file as `# warning:` lines, so a committed file keeps them. `scripts/perf bounds` lists them again at the end.
   - `tests/bounds` checks the pruning, segment, mask and cost logic on a small generated file whose every number is worked out by hand (`cargo test --test bounds`).

### Reading the table

The terminal shows the main columns in MB; the TSV has all of them, in bytes. `filters` counts the pushed filters, with how many statistics can't use. `row_groups` and `pages` are kept/total. `matching` and `sel_%` are rows passing the scan's own filters. On the `TOTAL` row, `output` is the query's result rows and `groups` the rows out of each final aggregate, outermost first; `-` marks an aggregate a `LIMIT` let DataFusion stop early (ClickBench q17), where the count would depend on thread timing. `meta_bytes` (TSV only) is the footer plus the page index of the needed chunks.

What the current data shows (2026-11-19): on TPC-H, statistics rule out no row group or page for any query, and late materialization and the oracle equal the reachable bound, because every page holds matches, so the work to win is decoding and filtering. The one saving is `o_shippriority` in q03, 0 in every row, which needs no read. Full ClickBench is clustered by `CounterID` and date: q36-q42 keep 3 of 226 row groups (a 98% cut), and q19 shows the most layout headroom (232 MB reachable, 1.5 MB oracle, for 4 matching rows). Most other ClickBench queries have no filter statistics can use.

Limits: uncompressed bytes count whole column chunks (a page's uncompressed size is only in its header), and values count whole pages.

### Worked example: TPC-H Q6, verified by hand

Worked out without the tool (2026-11-17, `lineitem.parquet` 183,992,906 bytes, SHA-256 prefix `59a4e6313fd17a51`), and the tool reproduces every number; DataFusion's own `bytes_scanned` is also 46,472,819:

| Measure | Value |
|---|---|
| Columns read | `l_shipdate`, `l_discount`, `l_quantity` (filters), `l_extendedprice` (output only) |
| Rows, row groups, data pages per column | 6,001,215, 53, 318 |
| Projection, compressed | 46,472,819 bytes: 28,256,256 in data pages, 18,216,563 in dictionary pages and headers |
| Projection, uncompressed | 74,285,345 bytes |
| Row groups / pages ruled out by statistics | 0 of 53 / 0 of 318 per filter column |
| Reachable minimum bytes read | 46,472,819, the whole projection |
| Matching rows | 114,160 (1.902%) |
| Pages holding a match | 318 of 318 in every column (fewest matches in one page: 172) |
| Oracle bytes | 46,472,819: no layout headroom at page granularity |
| Result | 123141078.2283 (DuckDB and DataFusion agree, and it is the published SF1 answer) |

Every row group and page spans the whole date range, because rows are in order-key order. On this data nothing can skip I/O for Q6, so its gains have to come from decoding and filtering faster.

How it was checked: chunk sizes and row-group statistics from DuckDB's `parquet_metadata()`; per-page row ranges, sizes and min/max read from the column and offset index with parquet 59, with rows split at every filter column's page boundary and a segment kept only if every filter's page may match; matches from DuckDB's `read_parquet(..., file_row_number=true)` mapped onto each column's pages. Dates are days since 1970 (1994-01-01 = 8766), and decimals are unscaled at scale 2.

## Layer 1: query timing

```bash
scripts/perf run tpch          # all 22 queries
scripts/perf run clickbench q20
scripts/perf compare           # newest run vs the one before it, same suite and settings
scripts/perf compare <old.tsv> <new.tsv>
```

Each query runs in its own process: once as a warmup, then 10 timed runs, then one more run that counts allocations. Counting is off during the timed runs, so timings come from the plain system allocator; counters updated by every thread on every allocation could otherwise slow down allocation-heavy parallel queries. Every run must return the same number of rows as the warmup, or the query is an error. Both choices were measured on TPC-H SF1 with DataFusion (2026-11-19): counting during the timed runs made queries 15-57% slower (Q6 33 ms against 16 ms), and running all queries in one process made them another 5-18% slower. Timings taken before then are not comparable. Progress shows live on the terminal, then a table:

| Column | Meaning |
|---|---|
| `status` | `ok`, `unsupported` (the engine rejects the SQL), or `error` (printed above the table) |
| `min_ms`, `median_ms` | Over the timed runs. Compare medians; min is the best case. With an even number of runs the median is the upper middle run, so it is always a measured time |
| `rows` | Rows returned: a check that two engines agree |
| `allocs`, `alloc_mib` | Allocations and bytes allocated in the counted run. A `realloc` counts as an allocation of its new size, so `Vec` growth shows |
| `peak_mib` | Most memory held at once during the counted run, above what was live when it started |

Results are saved as `bench-results/<suite>/<label>/<date>-<time>-<commit>.tsv`. The label is the engine, plus thread count and profile when not the defaults, so `compare` never mixes settings by accident. `-dirty` in the name means there were uncommitted changes or new files.

`compare`'s `check` column flags any query whose `status` or `rows` changed between the two runs: a faster query that returns different rows is a bug, not a win.

**When:** before and after every change you expect to affect speed or memory.

**Noise:** this is a laptop. Two identical runs of Q6 differed by 3.5%, so treat changes under about 5% as noise unless they repeat. Plug in power, turn off Low Power Mode, and wait for builds to finish before measuring: Low Power Mode alone cut measured memory bandwidth by more than half. `scripts/perf` warns about battery and Low Power Mode. If a result is surprising, run it again.

### Reference: DuckDB on ClickBench

Measured 2026-11-14 with DuckDB 1.4.3, warm OS cache, 5 runs in one session, best run. Strings read with `binary_as_string=true`. The shell's timer resolves to 1 ms, so the small-file integer queries are at its floor. The first run of a session is slower (702 ms against 515 ms for `LIKE` on 100 files); this engine targets a cold start, so the first run is the fairer comparison.

| Query | Small file, 1 thread | Small file, 14 threads | 100 files, 14 threads | Single file, 14 threads |
|---|---|---|---|---|
| `COUNT(*)` | about 1 ms | about 1 ms | 23 ms | 24 ms |
| `COUNT(*) WHERE AdvEngineID <> 0` | 1 to 2 ms | 1 to 2 ms | 22 ms | 41 ms |
| `SUM(AdvEngineID), COUNT(*), AVG(ResolutionWidth)` | 4 to 5 ms | 3 ms | 52 ms | 53 ms |
| `COUNT(*) WHERE URL LIKE '%google%'` | 54 ms | 37 ms | 515 ms | 536 ms |

Their results, for checking another engine's answers:

| Query | Small file | Full data |
|---|---|---|
| `COUNT(*)` | 1,000,000 | 99,997,497 |
| `COUNT(*) WHERE AdvEngineID <> 0` | 14,174 | 630,500 |
| `SUM(AdvEngineID), COUNT(*), AVG(ResolutionWidth)` | 80778, 1000000, 1604.08959 | 7280088, 99997497, 1513.4879349030107 |
| `COUNT(*) WHERE URL LIKE '%google%'` | 95 | 15,911 |

## Layer 2: CPU profile

```bash
scripts/perf profile tpch q06        # 5 s
scripts/perf profile tpch q06 20     # 20 s, for short queries or more detail
```

Runs one query in a loop under samply, which opens the Firefox Profiler in the browser when it finishes. The profile is saved in `bench-results/profiles/`; reopen one with `samply load <file>`. Use the flame graph or call tree, and look for:

- **Wide frames you didn't expect:** that's where the time goes.
- **`malloc`, `free`, `realloc`:** allocation cost (see layer 3).
- **`memcpy`, `memmove`:** copies.
- **Page-fault or kernel frames under large allocations:** fresh memory from the OS, which reusing buffers avoids.

**When:** after layer 1 shows a query is slow or got slower, to find out why.

## Layer 3: heap profile

```bash
scripts/perf heap tpch q06           # 1 profiled run
scripts/perf heap tpch q06 3         # 3 runs; totals are summed
```

Records a backtrace for every allocation, so it's slow: about 8 s per run for Q6. It doesn't report timings. The warmup run isn't profiled, so one-time setup stays out. One run is enough because a query allocates the same way every time, and the per-site totals are then exactly one query's worth.

When it finishes, the dhat viewer opens and the file's path is on the clipboard. Click **Load…**, press Cmd+Shift+G in the file picker, paste, and press Return. The file stays on your machine. Then use **Sort metric**:

| Sort by | Finds |
|---|---|
| Total (bytes) | Who allocates the most data: copies and intermediate arrays |
| Total (blocks) | Who allocates most often: per-row or per-value allocations |
| At t-gmax (bytes) | What is held at peak memory |
| Total (blocks), short-lived | Buffers allocated and freed right away: candidates for reuse |

Expand an entry to see its call stack. Several blocks from one site with doubling sizes means a `Vec` growing without `with_capacity`.

**Trust function names first, then lines, and inlined frames least.** Each frame shows a function and a `file:line`. Two things can make the location wrong:

- **Fat LTO** (`PROFILE=release-profiling`) often pairs a frame with the wrong file. The line and column stay right, so `from_iter` at `projection_pushdown.rs:33:9` really means std's `spec_from_iter.rs:33:9`. A Q6 profile had 534 such frames with fat LTO and none with the default `bench` profile, so profile heap usage with `bench`. The script warns if you don't.
- **No line info:** code the compiler couldn't tie to a source line, about 4% of frames. The script shows these as `(no line info)`, keeping the function name.

Inlined frames, the std and iterator functions nested inside the caller, are reconstructed from debug info, so read a chain of them as "somewhere in this call path" rather than an exact location.

**When:** after layers 1 or 2 point to allocation (high `allocs` or `alloc_mib`, or `malloc` or `memcpy` in the profile) and you need to know which code is responsible.

## Settings

Set as environment variables before the command, for example `ENGINE=ours THREADS=1 scripts/perf run tpch`.

| Variable | Default | Meaning |
|---|---|---|
| `ENGINE` | `datafusion` | `ours` or `datafusion`. DataFusion is the reference until this engine runs queries end to end; then the default switches to `ours` |
| `THREADS` | all cores | DataFusion partitions and worker threads. `THREADS=1` for single-core numbers |
| `PROFILE` | `bench` | `bench` is thin LTO: a relink took about 4 minutes when measured. `release-profiling` is fat LTO and matches a release build, but every relink took about 10 minutes. Use it for numbers you write down. The speed difference between the two hasn't been measured on a quiet machine yet |

To compare engines, run the suite once with each and give `compare` both files.

## Reading allocation numbers

Raw totals mean nothing on their own. Divide by what the query has to do:

| Ratio | How to compute | Good | Bad, and what it means |
|---|---|---|---|
| Allocations per row | `allocs` ÷ rows scanned | well under 0.01: a few per batch | near 1: an allocation per value |
| Byte amplification | `alloc_mib` ÷ decoded size of the columns read | 1–3× | 5× or more: copies, or a new array for each expression step |
| Peak | `peak_mib` against the decoded input | follows thread count, not input size | grows with input: something holds data it should stream. Expected for sorts, hash-join builds and large GROUP BYs, where the peak should be close to what they hold |

Examples from DataFusion:

- **Q6:** 0.03 allocations per row, 2.5× amplification, and a 38 MiB peak against 298 MiB of input. All healthy.
- **Q1:** 4.4× amplification, because each arithmetic step creates a new array.
- **ClickBench q00 (`COUNT(*)`):** about 4,000 allocations with almost no data, which is DataFusion's fixed cost per query. Subtract it before computing ratios for small queries.

An allocation problem is worth fixing only if it costs time. If `malloc`, `free`, `memcpy` and page faults together take a few percent in the layer 2 profile, the ratios don't matter.

## Common workflows

**Did my change help?** `scripts/perf run <suite>` before and after, then `scripts/perf compare`.

**Why is this query slow?** `run` it to confirm, `profile` it to see where the time goes, then `heap` it if the profile shows allocation or copying.

**Why does this query use so much memory?** Check `peak_mib` in `run`, then `heap` and sort by At t-gmax.

**New machine?** `scripts/perf limits`, and update the table in [Layer 0](#layer-0-hardware-limits).

## Files

```
scripts/perf                 the entry point
benches/query.rs             layer 1: timing, plus the counting allocator
benches/heap.rs              layer 3: dhat
benches/limits.rs            layer 0
benches/bounds/              bounds: plan, metadata, pruning, data pass, DataFusion and DuckDB checks
benches/reference/bounds/    committed bounds, one per reference dataset
benches/common/mod.rs        shared by the query, heap and bounds benches: arguments, engines, query loading, progress
benches/queries/             query suites and setup views (sources and license in its README.md)
tests/bounds/                tests for the bounds logic, on a generated file
bench-results/               saved runs, gitignored
```

**Adding a query:** drop a `.sql` file into `benches/queries/<suite>/`. The file stem is its name. A file can hold several `;`-separated statements, as TPC-H Q15 does.

**Adding a suite:** a directory of `.sql` files, plus a case in `bench_args` in `scripts/perf` that registers its tables, and in `dataset_of` for its committed bounds.
