//! End-to-end query timings and allocation counts, for this engine or
//! DataFusion. Arguments are in `common/mod.rs`.
//!
//! Registers the tables, then runs each query once as a warmup, then the
//! timed runs, then one more run that counts allocations. Opening the tables
//! is not timed. Counting is off during the timed runs, so the timings come
//! from the plain system allocator: with every thread updating the same
//! counters, counting could otherwise slow down allocation-heavy parallel
//! queries.
//!
//! Prints TSV: a header, then one row per query. Every run must return the
//! same number of rows, or the query reports an error. Allocation columns are
//! from the counted run. A `realloc` counts as one allocation of its new size,
//! so `Vec` growth shows up. `peak_mib` is the most memory held at once during
//! that run, above what was live when it started.
//!
//! Usually run through `scripts/perf`, which starts a fresh process per
//! query. Directly:
//!
//! ```text
//! cargo bench --bench query -- --engine datafusion --queries benches/queries/clickbench \
//!     --table hits=clickbench-data/partitioned/hits_0.parquet
//! ```

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::error::Error;
use std::hint::black_box;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use common::{Engine, Failure, NamedQuery, Progress, Repeat};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Whether allocations are being counted. Off except during the counted run;
/// while off, each allocation pays one load of a value no thread writes
static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);
/// Bytes allocated minus bytes freed since counting started. Negative when
/// the run frees more memory from before it than it allocates
static LIVE_BYTES: AtomicI64 = AtomicI64::new(0);
static PEAK_BYTES: AtomicI64 = AtomicI64::new(0);

/// The system allocator, plus counters that run only while `COUNTING` is on
struct Counting;

fn bytes_i64(bytes: usize) -> i64 {
    i64::try_from(bytes).unwrap_or(i64::MAX)
}

fn counted_alloc(bytes: usize) {
    if COUNTING.load(Relaxed) {
        ALLOCS.fetch_add(1, Relaxed);
        ALLOC_BYTES.fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Relaxed);
        let bytes = bytes_i64(bytes);
        let live = LIVE_BYTES.fetch_add(bytes, Relaxed).saturating_add(bytes);
        PEAK_BYTES.fetch_max(live, Relaxed);
    }
}

fn counted_free(bytes: usize) {
    if COUNTING.load(Relaxed) {
        LIVE_BYTES.fetch_sub(bytes_i64(bytes), Relaxed);
    }
}

#[expect(
    unsafe_code,
    reason = "a global allocator can only be written with `unsafe`; this bench only"
)]
// SAFETY: every method passes its arguments to `System` unchanged and returns
// its result, so `System`'s guarantees hold. The counters never touch the memory.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract for `layout`
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            counted_alloc(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `GlobalAlloc::alloc_zeroed`'s contract for `layout`
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            counted_alloc(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from this allocator, which got it from `System`
        unsafe { System.dealloc(ptr, layout) };
        counted_free(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr` came from `System` via this allocator, and the caller
        // upholds `GlobalAlloc::realloc`'s contract for `layout` and `new_size`
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            counted_free(layout.size());
            counted_alloc(new_size);
        }
        new
    }
}

/// What the counted run allocated
struct Allocations {
    count: u64,
    bytes: u64,
    /// Most bytes held at once above the start
    peak: u64,
}

/// Runs `f` with allocation counting on, from zeroed counters
fn count_allocations<T>(f: impl FnOnce() -> T) -> (T, Allocations) {
    ALLOCS.store(0, Relaxed);
    ALLOC_BYTES.store(0, Relaxed);
    LIVE_BYTES.store(0, Relaxed);
    PEAK_BYTES.store(0, Relaxed);
    COUNTING.store(true, Relaxed);
    let out = black_box(f());
    COUNTING.store(false, Relaxed);
    let counted = Allocations {
        count: ALLOCS.load(Relaxed),
        bytes: ALLOC_BYTES.load(Relaxed),
        peak: u64::try_from(PEAK_BYTES.load(Relaxed)).unwrap_or(0),
    };
    (out, counted)
}

fn main() -> Result<(), Box<dyn Error>> {
    // `cargo nextest` lists tests with `--list`; this target has none.
    if std::env::args().any(|arg| arg == "--list") {
        return Ok(());
    }
    let bench = common::setup(std::env::args().skip(1))?;

    let mut out = io::stdout().lock();
    writeln!(
        out,
        "query\tstatus\truns\tmin_ms\tmedian_ms\trows\tallocs\talloc_mib\tpeak_mib"
    )?;
    for (i, query) in bench.queries.iter().enumerate() {
        let progress = bench.progress(i);
        let row =
            measure(&bench.engine, query, bench.repeat, &progress).unwrap_or_else(|failure| {
                let status = match failure {
                    Failure::Unsupported => {
                        progress.finish("unsupported");
                        "unsupported"
                    }
                    Failure::Error(error) => {
                        progress.finish(&format!("error: {error}"));
                        "error"
                    }
                };
                format!("{}\t{status}\t-\t-\t-\t-\t-\t-\t-", query.name)
            });
        writeln!(out, "{row}")?;
        out.flush()?;
    }
    Ok(())
}

/// One TSV row for `query`, reporting progress and the result on stderr
fn measure(
    engine: &Engine,
    query: &NamedQuery,
    repeat: Repeat,
    progress: &Progress,
) -> Result<String, Failure> {
    // Warmup, and finds out whether the engine supports the query at all
    progress.step("warmup");
    let rows = engine.execute(&query.sql)?;
    let same_rows = |run_rows: usize, run: &str| {
        if run_rows == rows {
            Ok(())
        } else {
            Err(Failure::Error(format!(
                "{run} returned {run_rows} rows, the warmup {rows}"
            )))
        }
    };

    let mut durations = Vec::new();
    repeat.each(|run| {
        // Outside the measured section: printing allocates
        progress.step(&repeat.describe(run));
        let started = Instant::now();
        let result = black_box(engine.execute(&query.sql));
        durations.push(started.elapsed());
        same_rows(result.map_err(after_warmup)?, &format!("run {run}"))
    })?;
    durations.sort_unstable();

    progress.step("counting allocations");
    let (result, allocs) = count_allocations(|| engine.execute(&query.sql));
    same_rows(result.map_err(after_warmup)?, "the counted run")?;

    // Upper middle for an even count, so it is always a measured duration
    let (min, median) = (durations[0], durations[durations.len() / 2]);
    let (bytes, peak) = (mib(allocs.bytes), mib(allocs.peak));
    progress.finish(&format!(
        "ok  median {:.1} ms  min {:.1} ms  {} runs  {} allocs  {bytes:.1} MiB",
        millis(median),
        millis(min),
        durations.len(),
        allocs.count,
    ));
    Ok(format!(
        "{}\tok\t{}\t{:.3}\t{:.3}\t{rows}\t{}\t{bytes:.2}\t{peak:.2}",
        query.name,
        durations.len(),
        millis(min),
        millis(median),
        allocs.count,
    ))
}

/// A run that fails after the warmup succeeded is an error, never "unsupported"
fn after_warmup(failure: Failure) -> Failure {
    match failure {
        Failure::Unsupported => Failure::Error("unsupported after the warmup".to_string()),
        error @ Failure::Error(_) => error,
    }
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

#[expect(
    clippy::cast_precision_loss,
    reason = "byte counts far below 2^53; shown to two decimals"
)]
fn mib(bytes: u64) -> f64 {
    bytes as f64 / f64::from(1u32 << 20)
}
