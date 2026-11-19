//! Shared by the `query` and `heap` benches: arguments, query loading, and
//! the engine under test.
//!
//! ```text
//! --queries <dir>        one query per `*.sql` file, named by file stem
//! --table <name=path>    register a Parquet file; repeat per table
//! --setup <file.sql>     DataFusion only: statements to run once after registering
//! --engine <name>        `ours` (default) or `datafusion`
//! --threads <n>          DataFusion partitions and worker threads (default: all cores)
//! --only <name>          run one query
//! --position <i>/<n>     with `--only`: show progress as query `i` of `n`
//! --runs <n>             timed runs per query (default 10)
//! --seconds <s>          instead of `--runs`, repeat for this long
//! ```
//!
//! `scripts/perf` runs each query in its own process with `--only` and
//! `--position`, so no query sees another's caches or allocator state.

use std::cell::Cell;
use std::error::Error;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use datafusion::arrow::array::RecordBatch;
use datafusion::error::DataFusionError;
use datafusion::prelude::{ParquetReadOptions, SessionConfig, SessionContext};
use query_engine::{EngineError, ParquetSource, QueryError, Session};
use tokio::runtime::Runtime;

const DEFAULT_RUNS: usize = 10;
const USAGE: &str = "usage: --queries <dir> --table <name=path>... [--engine ours|datafusion] \
                     [--threads <n>] [--only <name> [--position <i>/<n>]] \
                     [--runs <n> | --seconds <s>]";

/// A parsed command line: engine with tables registered, and queries to run
pub(crate) struct Bench {
    pub(crate) engine: Engine,
    pub(crate) queries: Vec<NamedQuery>,
    pub(crate) repeat: Repeat,
    /// From `--position`: this run's one query is query `.0` of `.1`
    position: Option<(usize, usize)>,
}

impl Bench {
    /// Progress for `self.queries[index]`, numbered by `--position` if given
    pub(crate) fn progress(&self, index: usize) -> Progress {
        let (number, total) = self.position.unwrap_or((index + 1, self.queries.len()));
        Progress::new(number, total, &self.queries[index].name)
    }
}

pub(crate) struct NamedQuery {
    pub(crate) name: String,
    pub(crate) sql: String,
}

/// How long to keep running a query
#[derive(Clone, Copy)]
pub(crate) enum Repeat {
    Runs(usize),
    For(Duration),
}

impl Repeat {
    /// Calls `run` with the run number (from 1) at least once, until the run
    /// count or time is used up, or `run` fails. Returns the number of runs
    pub(crate) fn each<E>(self, mut run: impl FnMut(usize) -> Result<(), E>) -> Result<usize, E> {
        let started = Instant::now();
        let mut runs = 0;
        loop {
            runs += 1;
            run(runs)?;
            let done = match self {
                Self::Runs(limit) => runs >= limit,
                Self::For(limit) => started.elapsed() >= limit,
            };
            if done {
                return Ok(runs);
            }
        }
    }

    /// Progress text for run `run`: `run 3/10` or `run 3 (5 s)`
    pub(crate) fn describe(self, run: usize) -> String {
        match self {
            Self::Runs(limit) => format!("run {run}/{limit}"),
            Self::For(limit) => format!("run {run} ({} s)", limit.as_secs_f64()),
        }
    }
}

/// Live progress for one query, on stderr so stdout stays TSV
///
/// On a terminal, `step` rewrites the query's line, at most every 100 ms so
/// fast loops don't pay for printing. Otherwise `step` prints only the first
/// step. `finish` always prints the query's final line
pub(crate) struct Progress {
    prefix: String,
    terminal: bool,
    last_step: Cell<Option<Instant>>,
}

const STEP_INTERVAL: Duration = Duration::from_millis(100);

#[expect(
    clippy::print_stderr,
    reason = "progress goes to stderr so stdout stays machine-readable"
)]
impl Progress {
    /// Progress for query `number` (from 1) of `total`
    fn new(number: usize, total: usize, name: &str) -> Self {
        let width = total.to_string().len();
        Self {
            prefix: format!("[{number:>width$}/{total}] {name}"),
            terminal: std::io::stderr().is_terminal(),
            last_step: Cell::new(None),
        }
    }

    /// What the query is doing now, e.g. `warmup` or `run 3/10`
    pub(crate) fn step(&self, what: &str) {
        let now = Instant::now();
        match self.last_step.get() {
            Some(_) if !self.terminal => return,
            Some(last) if now.duration_since(last) < STEP_INTERVAL => return,
            _ => {}
        }
        self.last_step.set(Some(now));
        if self.terminal {
            eprint!("\r\x1b[2K{} {what}", self.prefix);
        } else {
            eprintln!("{} {what}", self.prefix);
        }
    }

    /// The query's result line
    pub(crate) fn finish(&self, what: &str) {
        if self.terminal {
            eprint!("\r\x1b[2K");
        }
        eprintln!("{} {what}", self.prefix);
    }
}

/// Why a query didn't run
pub(crate) enum Failure {
    /// The engine doesn't support this SQL yet
    Unsupported,
    Error(String),
}

/// The engine under test, with tables registered
pub(crate) enum Engine {
    Ours(Session),
    DataFusion {
        ctx: Box<SessionContext>,
        runtime: Runtime,
    },
}

impl Engine {
    fn open(
        name: &str,
        tables: &[(String, PathBuf)],
        setup: Option<&str>,
        threads: Option<usize>,
    ) -> Result<Self, Box<dyn Error>> {
        match name {
            "ours" => {
                if setup.is_some() {
                    return Err("--setup is DataFusion only".into());
                }
                let mut session = Session::new();
                for (table, path) in tables {
                    let source = ParquetSource::open(path).map_err(|err| {
                        format!("opening table `{table}` at {}: {err}", path.display())
                    })?;
                    session.register(table.clone(), source)?;
                }
                Ok(Self::Ours(session))
            }
            "datafusion" => {
                let mut runtime = tokio::runtime::Builder::new_multi_thread();
                let mut config = SessionConfig::new();
                if let Some(threads) = threads {
                    runtime.worker_threads(threads);
                    config = config.with_target_partitions(threads);
                }
                // ClickBench stores strings as unannotated BYTE_ARRAY
                config.options_mut().execution.parquet.binary_as_string = true;
                let runtime = runtime.enable_all().build()?;
                let ctx = SessionContext::new_with_config(config);
                for (table, path) in tables {
                    let path = path.to_str().ok_or("table path is not UTF-8")?;
                    runtime.block_on(ctx.register_parquet(
                        table.as_str(),
                        path,
                        ParquetReadOptions::default(),
                    ))?;
                }
                if let Some(setup) = setup {
                    runtime.block_on(run_statements(&ctx, setup))?;
                }
                Ok(Self::DataFusion {
                    ctx: Box::new(ctx),
                    runtime,
                })
            }
            other => Err(format!("unknown engine `{other}` (ours, datafusion)").into()),
        }
    }

    /// Runs `sql` to completion, drops the result, and returns its row count.
    /// For DataFusion, `sql` may hold several `;`-separated statements (TPC-H
    /// Q15 creates and drops a view); rows are summed over all of them
    pub(crate) fn execute(&self, sql: &str) -> Result<usize, Failure> {
        match self {
            Self::Ours(session) => match session.execute(sql) {
                Ok(result) => Ok(result.num_rows()),
                Err(EngineError::Query(QueryError::Unsupported(_))) => Err(Failure::Unsupported),
                Err(err) => Err(Failure::Error(err.to_string())),
            },
            Self::DataFusion { ctx, runtime } => runtime
                .block_on(run_statements(ctx, sql))
                .map_err(|err| match err {
                    DataFusionError::NotImplemented(_) => Failure::Unsupported,
                    other => Failure::Error(other.to_string()),
                }),
        }
    }
}

/// The `;`-separated statements in `sql`. Lines starting with `--` are dropped
/// first, so a `;` in a comment can't split a statement. A `;` inside a string
/// literal would; none of the queries have one
pub(crate) fn statements(sql: &str) -> Vec<String> {
    let code = sql
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    code.split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .map(str::to_string)
        .collect()
}

/// Runs each statement in `sql` (see [`statements`]), returning total rows
async fn run_statements(ctx: &SessionContext, sql: &str) -> Result<usize, DataFusionError> {
    let mut rows = 0;
    for statement in statements(sql) {
        let batches = ctx.sql(&statement).await?.collect().await?;
        rows += batches.iter().map(RecordBatch::num_rows).sum::<usize>();
    }
    Ok(rows)
}

/// Parses `args` (the command line without the program name), opens the
/// engine, and loads the queries
pub(crate) fn setup(args: impl IntoIterator<Item = String>) -> Result<Bench, Box<dyn Error>> {
    let mut dir = None;
    let mut tables = Vec::new();
    let mut setup = None;
    let mut engine = "ours".to_string();
    let mut threads = None;
    let mut only = None;
    let mut position = None;
    let mut repeat = Repeat::Runs(DEFAULT_RUNS);

    // The test runner appends `--bench` when `cargo bench` launches a target.
    let mut args = args.into_iter().filter(|arg| arg != "--bench");
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--queries" => dir = Some(PathBuf::from(value()?)),
            "--table" => {
                let spec = value()?;
                let (name, path) = spec
                    .split_once('=')
                    .ok_or_else(|| format!("--table wants name=path, got `{spec}`"))?;
                tables.push((name.to_string(), PathBuf::from(path)));
            }
            "--setup" => setup = Some(std::fs::read_to_string(value()?)?),
            "--engine" => engine = value()?,
            "--threads" => threads = Some(value()?.parse()?),
            "--only" => only = Some(value()?),
            "--position" => {
                let spec = value()?;
                let parsed = spec
                    .split_once('/')
                    .and_then(|(i, n)| Some((i.parse().ok()?, n.parse().ok()?)))
                    .filter(|&(i, n)| (1..=n).contains(&i));
                position =
                    Some(parsed.ok_or_else(|| format!("--position wants i/n, got `{spec}`"))?);
            }
            "--runs" => repeat = Repeat::Runs(value()?.parse()?),
            "--seconds" => repeat = Repeat::For(Duration::from_secs_f64(value()?.parse()?)),
            _ => return Err(format!("unknown argument `{flag}`\n{USAGE}").into()),
        }
    }

    let dir = dir.ok_or(USAGE)?;
    if position.is_some() && only.is_none() {
        return Err("--position needs --only".into());
    }
    let mut queries = load_queries(&dir)?;
    if let Some(only) = &only {
        queries.retain(|query| &query.name == only);
        if queries.is_empty() {
            return Err(format!("no query named `{only}` in {}", dir.display()).into());
        }
    }

    Ok(Bench {
        engine: Engine::open(&engine, &tables, setup.as_deref(), threads)?,
        queries,
        repeat,
        position,
    })
}

fn load_queries(dir: &Path) -> std::io::Result<Vec<NamedQuery>> {
    let mut files = std::fs::read_dir(dir)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    files.retain(|file| file.extension().is_some_and(|ext| ext == "sql"));
    files.sort();
    files
        .iter()
        .map(|file| {
            Ok(NamedQuery {
                name: file
                    .file_stem()
                    .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned()),
                sql: std::fs::read_to_string(file)?,
            })
        })
        .collect()
}
