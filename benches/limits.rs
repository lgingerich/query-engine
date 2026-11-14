//! Hardware limits a query over `file` cannot beat on this machine.
//!
//! * `read_file` — read the whole file sequentially into one reused buffer.
//!   When the file is warm this measures the OS page cache, not the disk; clear
//!   the cache first (`purge` on macOS, `drop_caches` on Linux) for the disk.
//! * `sum_1thread` — sum a 1 GiB `u64` array on one thread: memory bandwidth
//!   for one core.
//! * `sum_all_threads` — the same sum split into one disjoint slice per
//!   available core, all at once: total memory bandwidth.
//!
//! Each prints min and median over several runs as `key=value` pairs. GB/s is
//! 10^9 bytes per second.
//!
//! ```text
//! cargo bench --bench limits -- clickbench-data/partitioned/hits_0.parquet
//! ```

use std::fs::File;
use std::hint::black_box;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

const RUNS: usize = 5;
const READ_BUFFER_BYTES: usize = 8 << 20;
const ARRAY_BYTES: usize = 1 << 30;

fn main() -> io::Result<()> {
    // `cargo nextest` lists tests with `--list`; this target has none.
    if std::env::args().any(|arg| arg == "--list") {
        return Ok(());
    }
    // The test runner appends `--bench` when `cargo bench` launches this target.
    let path = std::env::args()
        .skip(1)
        .find(|arg| arg != "--bench")
        .ok_or_else(|| io::Error::other("usage: cargo bench --bench limits -- <file>"))?;
    let mut out = io::stdout().lock();
    writeln!(out, "optimized={}", !cfg!(debug_assertions))?;
    writeln!(out, "runs={RUNS}")?;

    let file_bytes = std::fs::metadata(&path)?.len();
    let mut buffer = vec![0u8; READ_BUFFER_BYTES];
    let mut failed = None;
    let read = time_runs(RUNS, || {
        if let Err(err) = read_whole(&path, &mut buffer) {
            failed = Some(err);
        }
    });
    if let Some(err) = failed {
        return Err(err);
    }
    writeln!(
        out,
        "measure=read_file file={path} {}",
        rates(file_bytes, &read)
    )?;
    writeln!(
        out,
        "note=read_file measures the OS page cache when the file is warm"
    )?;

    // `vec![1; n]` writes every element, so all pages are resident before timing.
    let data = vec![1u64; ARRAY_BYTES / size_of::<u64>()];
    let bytes = u64::try_from(ARRAY_BYTES).unwrap_or(u64::MAX);

    let one = time_runs(RUNS, || sum(black_box(&data)));
    writeln!(out, "measure=sum_1thread {}", rates(bytes, &one))?;

    let threads = std::thread::available_parallelism()?.get();
    let chunk = data.len().div_ceil(threads);
    // Thread spawn and join are inside the timed region; at 1 GiB they are small next to the sum.
    let all = time_runs(RUNS, || {
        std::thread::scope(|scope| {
            for slice in data.chunks(chunk) {
                scope.spawn(move || black_box(sum(black_box(slice))));
            }
        });
    });
    writeln!(
        out,
        "measure=sum_all_threads threads={threads} {}",
        rates(bytes, &all)
    )?;

    Ok(())
}

/// Durations of repeated runs, sorted fastest first.
struct Timings(Vec<Duration>);

impl Timings {
    fn min(&self) -> Duration {
        self.0[0]
    }

    /// The upper middle run, so it is always a duration that was measured.
    fn median(&self) -> Duration {
        self.0[self.0.len() / 2]
    }
}

/// Runs `f` `runs` times (at least once). Each result goes through `black_box`
/// so the work is not optimised away, and is dropped outside the timed region.
fn time_runs<T>(runs: usize, mut f: impl FnMut() -> T) -> Timings {
    let mut durations: Vec<Duration> = (0..runs.max(1))
        .map(|_| {
            let started = Instant::now();
            let out = black_box(f());
            let elapsed = started.elapsed();
            drop(out);
            elapsed
        })
        .collect();
    durations.sort_unstable();
    Timings(durations)
}

fn read_whole(path: &str, buffer: &mut [u8]) -> io::Result<u64> {
    let mut file = File::open(path)?;
    let mut total = 0u64;
    loop {
        let n = file.read(buffer)?;
        if n == 0 {
            return Ok(black_box(total));
        }
        black_box(&buffer[..n]);
        total += u64::try_from(n).unwrap_or(0);
    }
}

fn sum(values: &[u64]) -> u64 {
    values.iter().copied().fold(0, u64::wrapping_add)
}

fn rates(bytes: u64, timings: &Timings) -> String {
    format!(
        "bytes={bytes} min_ms={:.3} median_ms={:.3} max_gbps={:.2} median_gbps={:.2}",
        millis(timings.min()),
        millis(timings.median()),
        gbps(bytes, timings.min()),
        gbps(bytes, timings.median()),
    )
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

#[expect(
    clippy::cast_precision_loss,
    reason = "byte counts far below 2^53; rates are approximate"
)]
fn gbps(bytes: u64, duration: Duration) -> f64 {
    bytes as f64 / duration.as_secs_f64() / 1e9
}
