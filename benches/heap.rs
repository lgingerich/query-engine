//! Per-call-site heap profile of one query with dhat. Arguments are in
//! `common/mod.rs`; `--only` must pick exactly one query.
//!
//! Runs the query once as a warmup, untracked, so one-time setup stays out of
//! the profile. Then profiles `--runs` runs (default 10). Writes
//! `dhat-heap.json` to the working directory, for
//! <https://nnethercote.github.io/dh_view/dh_view.html>, which loads the file
//! in the browser without uploading it.
//!
//! dhat records a backtrace per allocation, so runs are much slower than in
//! the `query` bench; don't read timings from here.
//!
//! Usually run through `scripts/perf heap`. Directly:
//!
//! ```text
//! cargo bench --bench heap -- --engine datafusion --queries benches/queries/tpch \
//!     --table lineitem=tpch-data/lineitem.parquet --only q06 --runs 1
//! ```

mod common;

use std::error::Error;

use common::Failure;

#[global_allocator]
static ALLOCATOR: dhat::Alloc = dhat::Alloc;

/// Frames kept per backtrace. dhat's default of 10 often stops inside the
/// allocator or async runtime before reaching the code that asked
const BACKTRACE_FRAMES: usize = 24;

fn main() -> Result<(), Box<dyn Error>> {
    // `cargo nextest` lists tests with `--list`; this target has none.
    if std::env::args().any(|arg| arg == "--list") {
        return Ok(());
    }
    let bench = common::setup(std::env::args().skip(1))?;
    let [query] = bench.queries.as_slice() else {
        return Err("pick one query with --only".into());
    };

    let run = || match bench.engine.execute(&query.sql) {
        Ok(_) => Ok(()),
        Err(Failure::Unsupported) => Err(format!("query {} is unsupported", query.name)),
        Err(Failure::Error(error)) => Err(format!("query {}: {error}", query.name)),
    };
    let progress = bench.progress(0);
    progress.step("warmup (not profiled)");
    run()?;

    let profiler = dhat::Profiler::builder()
        .trim_backtraces(Some(BACKTRACE_FRAMES))
        .build();
    let result = bench.repeat.each(|number| {
        progress.step(&format!("profiling {}", bench.repeat.describe(number)));
        run()
    });
    match &result {
        Ok(runs) => progress.finish(&format!("profiled {runs} run(s), writing dhat-heap.json")),
        Err(error) => progress.finish(&format!("failed: {error}; writing dhat-heap.json")),
    }
    // Writes `dhat-heap.json` and prints a summary to stderr
    drop(profiler);
    result?;
    Ok(())
}
