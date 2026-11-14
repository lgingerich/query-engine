# Ideas: what this engine is and why


My ideas:
- Seems unlikely that data pages inside single chunk have different encodings, even though it's possible. Optimistically assume same encoding and fail if wrong.



This file is what the engine is trying to be and the ideas it is built on. `PARQUET.md` covers the file format. `NUMBERS.md` holds the measurements.

## The goal

An analytical query engine, written from scratch in Rust, that reads Parquet files it did not write and answers queries over them as fast as the hardware allows. It is built one layer at a time, and each layer is measured against a known limit before the next one is added.

## Where the time goes

Every analytical query pays for some mix of four things: fetching bytes, decompressing them, decoding them into values, and computing on those values. That gives three ways to be faster:

- **Run at the hardware's limit** on the bytes that must be processed.
- **Process fewer bytes** by proving that a file, row group, page or column is not needed.
- **Do less work per byte**: compute on the encoded form, and never build values a filter will discard.

Early measurements say reading bytes is the cheap part. On one core, decompression is several times slower than memory can deliver data, so it is usually the largest single cost. The order of payoff is: skip work, use every core, avoid building unneeded values, and only then tune how bytes are fetched.

## Core ideas

1. **Skip before working.** Use the cheapest evidence first (footer statistics, then bloom filters, page statistics and dictionaries) to rule data out before decompressing it. The engine must be fast when that evidence is missing, and faster when it is there.
2. **Compute on encoded data when it is cheaper.** A filter on a dictionary column can be evaluated once per distinct value instead of once per row. Every encoding can always fall back to plain decoded values, so supporting a new encoding never touches every operator.
3. **Late materialization.** Evaluate filters first, on only the filter columns, and decode the other columns only for the rows that survived.
4. **Fit the cache.** Work in small vectors that stay in the CPU's fastest caches, use data while it is still hot, reuse buffers, and size structures like hash tables to a cache level on purpose. Inner loops stay simple enough for the compiler to vectorize them.
5. **Parallelism belongs to the scheduler.** Work is split into small morsels in a shared queue, and every thread takes the next one when it is free. No fixed partitions, so uneven data cannot leave threads idle. Threads keep their own state and memory close to them, so the design carries over to multi-socket (NUMA) servers.
6. **Fetch ahead of the CPU.** Once pruning knows which byte ranges are needed, request them in the order they will be used, within a memory budget, through one interface for local disks and object storage.
7. **Robust plans over clever plans (later).** For joins, passing compact summaries of surviving keys between tables before joining makes a bad join order far less costly. The scan accepts filters that arrive while it is running so this can be added.

## Honest expectations

- Decompression is a wall on Parquet. Only skipping a page avoids it.
- Parquet offers few encodings to exploit, so encoded execution pays most on filters and low-cardinality grouping.
- Dictionary codes only mean something within one column chunk, so encoded data rarely travels far above the scan.
- Metadata is not free. Footers of large files take real time to decode, and many small files turn opening them into the cost.
- Fast repeated queries eventually need a cache of decoded data. That is deferred, but the in-memory format is designed so it could be kept.

## What "optimal" means

Every layer is judged against a limit that can be measured: the disk's sequential read rate, memory bandwidth, the codec's decompression speed, the time to read only the metadata, the fewest pages any engine could touch for that query, and linear scaling until a shared resource saturates. The gap between the engine and those limits is the to-do list.

## How to build it

- One layer at a time, each ending with a program that runs and a number.
- Every feature is driven by a real query.
- Measure, then change one thing.
- Check every answer against an existing engine.
- The simple correct version comes first and stays as the reference for the fast one.

## Decisions

- Parquet first. No custom file format and no Vortex for now.
- ClickBench is the main benchmark. TPC-H returns with joins.
- Linux is the deployment target; macOS is for development.
- Cold performance first: no engine-side caching.
- Portable code and auto-vectorization only.
- io_uring is built and measured as one I/O backend among several.
- SQL parsing and logical planning are borrowed until late.
- Storage grows in layers: one small file, one large file on all cores, many local files, object storage, table formats.

## Deferred

Incremental view maintenance, GPUs, JIT compilation, nested types, writing files, result and data caches, an internal storage format, hardening against malformed files, multi-query resource limits.
