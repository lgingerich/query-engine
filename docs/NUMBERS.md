# Numbers

Measured tables for this machine, with how each was taken.

**Machine:** Apple M3 Max, 14 cores (10 performance, 4 efficiency), 36 GiB RAM, macOS 26.6.2, Rust 1.100.0, release build. Laptop numbers, for comparing versions of this engine.

**Data:** ClickBench `hits`, in `clickbench-data/`.

- `hits.parquet`: one file, 14,779,976,446 bytes, 99,997,497 rows, 226 row groups.
- `partitioned/hits_0.parquet` to `hits_99.parquet`: 100 files. The Stage 1 file is `hits_0.parquet`: 122,446,530 bytes, 1,000,000 rows, 2 row groups (450,560 and 549,440).

## Hardware limits

`cargo bench --bench limits -- clickbench-data/partitioned/hits_0.parquet`, 5 runs, median. 2026-11-13.

| Limit | Result |
|---|---|
| Read the small file from the OS cache | 7.2 ms, 16.9 GB/s |
| Memory bandwidth, 1 thread (sum 1 GiB of `u64`) | 83.8 GB/s |
| Memory bandwidth, 14 threads | 123 GB/s |

The read is the OS cache. A cold read of the whole file has not been taken. One thread already reaches about two thirds of the machine's memory bandwidth.

## Footer open and parse

`parquet` crate 60.1.0. 2026-11-13. One shot after `purge` for the cold columns, then the min is ignored and the median of 5 cached runs is the warm column. Cold read is `open` + the 8-byte tail + the footer bytes. Parse is `ParquetMetaDataReader::decode_metadata` on those bytes. `open+parse` is `parse_and_finish`, which is what a normal open does. The parser was already resident, so the cold parse is decode time and not code being faulted back in.

These files have no page index (`column_index_offset` and `offset_index_offset` are absent on every column chunk).

| File | Footer | Cold read | Cold parse | Warm read | Warm parse | Warm open+parse |
|---|---|---|---|---|---|---|
| `hits_0.parquet` | 23,946 B | 0.49 ms | 0.08 ms | 0.02 ms | 0.07 ms | 0.08 ms |
| `hits.parquet` | 2,439,316 B | 1.0 ms | 6.6 ms | 0.44 ms | 5.8 ms | 6.7 ms |
| 100 partitions | 3,683,826 B | 39 ms | 11 ms | 2.0 ms | 11 ms | 13 ms |

Partition footers range from 22,863 to 57,061 bytes, median 34,546.

The footer is about 11 KB per row group on this 105-column schema (2.44 MB / 226), around 100 bytes of metadata per column chunk, plus the schema once per file. Parse time follows those bytes, at roughly 400 MB/s: 24 KB is 0.07 ms, 2.4 MB is about 6 ms. A hundred files cost more per byte than one footer, because each file repeats the schema and is decoded on its own.

A cold read does not follow the byte count. The 24 KB footer and the 2.4 MB footer are both about a millisecond, which is a seek plus a short transfer. The hundred files are the same seek a hundred times, about 0.4 ms each. Once the tail is cached, parsing is the whole cost for one file. Across many files the opens dominate until the cache is warm.

## DuckDB reference

DuckDB 1.4.3, warm OS cache, 5 runs in one session, best run. Strings read with `binary_as_string=true`. The shell's timer resolves to 1 ms, so the small-file integer queries are at its floor. The first run of a session is slower (702 ms against 515 ms for `LIKE` on 100 files); this engine targets a cold start, so the first run is the fairer comparison. DataFusion has not been timed yet.

| Query | Small file, 1 thread | Small file, 14 threads | 100 files, 14 threads | Single file, 14 threads |
|---|---|---|---|---|
| `COUNT(*)` | about 1 ms | about 1 ms | 23 ms | 24 ms |
| `COUNT(*) WHERE AdvEngineID <> 0` | 1 to 2 ms | 1 to 2 ms | 22 ms | 41 ms |
| `SUM(AdvEngineID), COUNT(*), AVG(ResolutionWidth)` | 4 to 5 ms | 3 ms | 52 ms | 53 ms |
| `COUNT(*) WHERE URL LIKE '%google%'` | 54 ms | 37 ms | 515 ms | 536 ms |

| Query | Small file | Full data |
|---|---|---|
| `COUNT(*)` | 1,000,000 | 99,997,497 |
| `COUNT(*) WHERE AdvEngineID <> 0` | 14,174 | 630,500 |
| `SUM(AdvEngineID), COUNT(*), AVG(ResolutionWidth)` | 80778, 1000000, 1604.08959 | 7280088, 99997497, 1513.4879349030107 |
| `COUNT(*) WHERE URL LIKE '%google%'` | 95 | 15,911 |
