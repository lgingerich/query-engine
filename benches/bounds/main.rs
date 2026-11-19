//! Theoretical bounds: the least work each query needs on these exact files.
//! Arguments are in `common/mod.rs`; the engine must be DataFusion, which
//! plans the queries and runs them for the cross-checks.
//!
//! For each query statement: DataFusion's optimized plan gives each table
//! scan's columns and pushed-down filters. The footer and page index give
//! sizes and statistics; `stats` decides from first principles what they rule
//! out; a data pass finds the rows that actually match. Two independent
//! checks: DataFusion pruning and reading each scan on its own, and DuckDB
//! counting the same matches and output rows.
//!
//! One argument of its own, before the common ones:
//!
//! ```text
//! --duckdb-setup <file.sql>   statements DuckDB runs after creating a view per table
//! ```
//!
//! Prints TSV: one row per table scan, then a `TOTAL` row per query, then a
//! `# warning: ...` line per warning, so a saved file keeps them. Progress
//! and warnings also go to stderr. `docs/BENCHMARKING.md` explains the columns.
//!
//! Usually run through `scripts/perf bounds`, which starts a fresh process
//! per query.

#[path = "../common/mod.rs"]
#[expect(
    dead_code,
    reason = "shared with the query and heap benches, which use the parts this one doesn't"
)]
mod common;
mod duckdb;
mod file;
mod measure;
mod oracle;
mod scan;
mod stats;

use std::collections::HashMap;
use std::error::Error;
use std::io::{self, Write};
use std::path::PathBuf;

use datafusion::prelude::SessionContext;

use common::{NamedQuery, Progress};
use file::ParquetFile;
use measure::{Bounds, Cost};
use scan::{DfScan, Scan};

const HEADER: &str = "query\ttable\tcolumns\tfilters\tunusable\trows\trow_groups\trg_kept\t\
                      pages\tpages_kept\tmeta_bytes\tproj_bytes\tproj_uncomp\treach_bytes\t\
                      reach_uncomp\tlate_bytes\toracle_bytes\treach_values\tlate_values\t\
                      oracle_values\tmatching\tselectivity_pct\toutput_rows\tgroups\t\
                      df_rg_pruned\tdf_pages_pruned\tdf_bytes\tduckdb";

fn main() -> Result<(), Box<dyn Error>> {
    // `cargo nextest` lists tests with `--list`; this target has none.
    if std::env::args().any(|arg| arg == "--list") {
        return Ok(());
    }
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let duckdb_setup = match args.iter().position(|arg| arg == "--duckdb-setup") {
        Some(at) => {
            let file = args
                .get(at + 1)
                .ok_or("--duckdb-setup needs a value")?
                .clone();
            args.drain(at..=at + 1);
            Some(std::fs::read_to_string(file)?)
        }
        None => None,
    };
    let bench = common::setup(args)?;
    let common::Engine::DataFusion { ctx, runtime } = &bench.engine else {
        return Err("bounds needs --engine datafusion".into());
    };

    let mut out = io::stdout().lock();
    writeln!(out, "{HEADER}")?;
    let mut files = HashMap::new();
    for (i, query) in bench.queries.iter().enumerate() {
        let progress = bench.progress(i);
        let rows = runtime.block_on(bound_query(
            ctx,
            query,
            duckdb_setup.as_deref(),
            &mut files,
            &progress,
        ))?;
        for row in rows {
            writeln!(out, "{row}")?;
        }
        out.flush()?;
    }
    Ok(())
}

/// TSV rows for one query: one per table scan, then its total
async fn bound_query(
    ctx: &SessionContext,
    query: &NamedQuery,
    duckdb_setup: Option<&str>,
    files: &mut HashMap<PathBuf, ParquetFile>,
    progress: &Progress,
) -> Result<Vec<String>, Box<dyn Error>> {
    let mut scans: Vec<Scan> = Vec::new();
    let mut output_rows = 0;
    let mut groups = Vec::new();
    // Every scan reads a base table, which stays registered after statements
    // such as TPC-H Q15's `DROP VIEW`, so scans are measured after all have run
    for statement in common::statements(&query.sql) {
        if !scan::is_query(&statement) {
            ctx.sql(&statement).await?.collect().await?;
            continue;
        }
        progress.step("planning and running in DataFusion");
        let run = scan::run(ctx, &statement).await?;
        output_rows += run.output_rows;
        groups.extend(run.groups);
        scans.extend(run.scans);
    }

    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut totals = Totals::default();
    let mut df_total = DfScan::default();
    for scan in &scans {
        if !files.contains_key(&scan.path) {
            files.insert(scan.path.clone(), ParquetFile::open(&scan.path)?);
        }
        let file = &files[&scan.path];
        let mask = if scan.filters.is_empty() {
            None
        } else {
            progress.step(&format!("data pass over {}", scan.table));
            match oracle::matches(ctx, scan, &scan.filter_columns(), file.rows()) {
                Ok(mask) => Some(mask),
                Err(err) => {
                    warn(
                        &mut warnings,
                        format!(
                            "query {}: no data pass for {}: {err}",
                            query.name, scan.table
                        ),
                    );
                    None
                }
            }
        };
        let bounds = measure::bound(scan, file, mask.as_ref())?;

        progress.step(&format!("DataFusion scan of {} alone", scan.table));
        let theirs = scan::datafusion_scan(ctx, scan).await?;
        let ours_pruned = bounds.row_groups - bounds.row_groups_kept;
        if theirs.row_groups_pruned > ours_pruned {
            warn(
                &mut warnings,
                format!(
                    "query {}: DataFusion pruned {} row groups of {}, the bounds {ours_pruned}: \
                 the pruning logic misses a case",
                    query.name, theirs.row_groups_pruned, scan.table
                ),
            );
        }
        if u64::try_from(theirs.bytes_scanned).unwrap_or(u64::MAX) < bounds.reachable.bytes {
            warn(
                &mut warnings,
                format!(
                    "query {}: DataFusion read {} bytes of {}, below the reachable bound {}: \
                 the pruning logic misses a case",
                    query.name, theirs.bytes_scanned, scan.table, bounds.reachable.bytes
                ),
            );
        }
        df_total.add(theirs);

        progress.step(&format!("DuckDB check for {}", scan.table));
        let check = duckdb::check_matches(scan, bounds.matching);
        rows.push(scan_row(&query.name, scan, &bounds, theirs, &check));
        totals.add(&bounds);
    }

    progress.step("DuckDB check of the output");
    let rows_check = duckdb::check_rows(&scans, &query.sql, duckdb_setup, output_rows);
    rows.push(totals.row(&query.name, output_rows, &groups, df_total, &rows_check));
    rows.extend(
        warnings
            .iter()
            .map(|warning| format!("# warning: {warning}")),
    );
    progress.finish(&format!(
        "projection {}  reachable {}  oracle {}  output {output_rows} rows  duckdb {rows_check}",
        mb(totals.projection.bytes),
        mb(totals.reachable.bytes),
        totals
            .oracle
            .map_or_else(|| "-".to_string(), |c| mb(c.bytes)),
    ));
    Ok(rows)
}

fn scan_row(query: &str, scan: &Scan, b: &Bounds, df: DfScan, duckdb: &str) -> String {
    let (pages, pages_kept) = b.pages.map_or_else(
        || ("-".to_string(), "-".to_string()),
        |(total, kept)| (total.to_string(), kept.to_string()),
    );
    let selectivity = b
        .matching
        .map_or_else(|| "-".to_string(), |m| format!("{:.3}", percent(m, b.rows)));
    [
        query.to_string(),
        scan.table.clone(),
        b.columns.to_string(),
        b.filters.to_string(),
        b.unusable.to_string(),
        b.rows.to_string(),
        b.row_groups.to_string(),
        b.row_groups_kept.to_string(),
        pages,
        pages_kept,
        b.meta_bytes.to_string(),
        b.projection.bytes.to_string(),
        b.projection.uncompressed.to_string(),
        b.reachable.bytes.to_string(),
        b.reachable.uncompressed.to_string(),
        opt(b.late.map(|c| c.bytes)),
        opt(b.oracle.map(|c| c.bytes)),
        b.reachable.values.to_string(),
        opt(b.late.map(|c| c.values)),
        opt(b.oracle.map(|c| c.values)),
        opt(b.matching),
        selectivity,
        "-".to_string(),
        "-".to_string(),
        df.row_groups_pruned.to_string(),
        df.pages_pruned.to_string(),
        df.bytes_scanned.to_string(),
        duckdb.to_string(),
    ]
    .join("\t")
}

/// Sums over a query's scans. Late and oracle are `None` if any scan lacks them
#[derive(Default)]
struct Totals {
    filters: usize,
    unusable: usize,
    meta_bytes: u64,
    projection: Cost,
    reachable: Cost,
    late: Option<Cost>,
    oracle: Option<Cost>,
    missing_data_pass: bool,
}

impl Totals {
    fn add(&mut self, b: &Bounds) {
        let sum = |into: &mut Cost, from: Cost| {
            into.bytes += from.bytes;
            into.uncompressed += from.uncompressed;
            into.values += from.values;
        };
        self.filters += b.filters;
        self.unusable += b.unusable;
        self.meta_bytes += b.meta_bytes;
        sum(&mut self.projection, b.projection);
        sum(&mut self.reachable, b.reachable);
        match (b.late, b.oracle) {
            (Some(late), Some(oracle)) => {
                sum(self.late.get_or_insert_default(), late);
                sum(self.oracle.get_or_insert_default(), oracle);
            }
            _ => self.missing_data_pass = true,
        }
    }

    fn row(
        &self,
        query: &str,
        output_rows: usize,
        groups: &[Option<usize>],
        df: DfScan,
        duckdb: &str,
    ) -> String {
        let (late, oracle) = if self.missing_data_pass {
            (None, None)
        } else {
            (self.late, self.oracle)
        };
        let groups = if groups.is_empty() {
            "-".to_string()
        } else {
            groups
                .iter()
                .map(|count| count.map_or_else(|| "-".to_string(), |c| c.to_string()))
                .collect::<Vec<_>>()
                .join(",")
        };
        let dash = || "-".to_string();
        [
            query.to_string(),
            "TOTAL".to_string(),
            dash(),
            self.filters.to_string(),
            self.unusable.to_string(),
            dash(),
            dash(),
            dash(),
            dash(),
            dash(),
            self.meta_bytes.to_string(),
            self.projection.bytes.to_string(),
            self.projection.uncompressed.to_string(),
            self.reachable.bytes.to_string(),
            self.reachable.uncompressed.to_string(),
            opt(late.map(|c| c.bytes)),
            opt(oracle.map(|c| c.bytes)),
            self.reachable.values.to_string(),
            opt(late.map(|c| c.values)),
            opt(oracle.map(|c| c.values)),
            dash(),
            dash(),
            output_rows.to_string(),
            groups,
            df.row_groups_pruned.to_string(),
            df.pages_pruned.to_string(),
            df.bytes_scanned.to_string(),
            duckdb.to_string(),
        ]
        .join("\t")
    }
}

fn opt(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_string(), |v| v.to_string())
}

#[expect(
    clippy::cast_precision_loss,
    reason = "row and byte counts far below 2^53; shown rounded"
)]
fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64 * 100.0
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "byte counts far below 2^53; shown rounded"
)]
fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

/// Shows `message` now, on stderr, and keeps it for the query's output
#[expect(
    clippy::print_stderr,
    reason = "warnings show on stderr as they happen; stdout gets them as comments"
)]
fn warn(warnings: &mut Vec<String>, message: String) {
    eprintln!("warning: {message}");
    warnings.push(message);
}
