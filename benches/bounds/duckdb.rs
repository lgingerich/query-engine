//! Cross-checks against the DuckDB CLI: an engine independent of DataFusion
//!
//! Each check returns `ok`, `diff:<duckdb's number>`, or `n/a` when DuckDB
//! isn't installed or can't run the SQL (for example DataFusion-only functions).

use std::fmt::Write;
use std::process::Command;

use datafusion::logical_expr::utils::conjunction;
use datafusion::sql::unparser::expr_to_sql;

use crate::common;
use crate::scan::{self, Scan};

/// Runs `sql` in a fresh DuckDB and returns stdout, or `None` on any failure
fn duckdb(sql: &str) -> Option<String> {
    let out = Command::new("duckdb")
        .args(["-csv", "-noheader", "-c", sql])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

fn read_parquet(scan: &Scan) -> String {
    format!(
        "read_parquet('{}', binary_as_string=true)",
        scan.path.display().to_string().replace('\'', "''")
    )
}

/// DuckDB's count of rows passing the scan's filters, against `ours`
pub(crate) fn check_matches(scan: &Scan, ours: Option<u64>) -> String {
    let Some(ours) = ours else {
        return "n/a".to_string();
    };
    let sql = match conjunction(scan.filters.iter().cloned()) {
        Some(filter) => match expr_to_sql(&filter) {
            Ok(filter) => format!("SELECT count(*) FROM {} WHERE {filter}", read_parquet(scan)),
            Err(_) => return "n/a".to_string(),
        },
        None => format!("SELECT count(*) FROM {}", read_parquet(scan)),
    };
    verdict(
        duckdb(&sql).and_then(|out| out.trim().parse::<u64>().ok()),
        ours,
    )
}

/// DuckDB's output row count for the whole query, against `ours`. Each table
/// the query scans becomes a view over its file, then `setup` runs (for
/// example ClickBench's `hits` view over `hits_raw`). Each query statement is
/// counted as `SELECT count(*) FROM (...)` and the counts are summed; other
/// statements, such as TPC-H Q15's view, run as written
pub(crate) fn check_rows(scans: &[Scan], sql: &str, setup: Option<&str>, ours: usize) -> String {
    let mut script = String::new();
    let mut seen = Vec::new();
    for scan in scans {
        if !seen.contains(&scan.table) {
            seen.push(scan.table.clone());
            // Writing to a `String` can't fail
            let _ = writeln!(
                script,
                "CREATE VIEW \"{}\" AS SELECT * FROM {};",
                scan.table.replace('"', "\"\""),
                read_parquet(scan)
            );
        }
    }
    if let Some(setup) = setup {
        script.push_str(setup);
        script.push('\n');
    }
    for statement in common::statements(sql) {
        if scan::is_query(&statement) {
            let _ = writeln!(script, "SELECT count(*) FROM ({statement});");
        } else {
            let _ = writeln!(script, "{statement};");
        }
    }
    let rows = duckdb(&script).and_then(|out| {
        out.lines()
            .map(|line| line.trim().parse::<u64>().ok())
            .sum::<Option<u64>>()
    });
    verdict(rows, u64::try_from(ours).unwrap_or(0))
}

fn verdict(theirs: Option<u64>, ours: u64) -> String {
    match theirs {
        Some(theirs) if theirs == ours => "ok".to_string(),
        Some(theirs) => format!("diff:{theirs}"),
        None => "n/a".to_string(),
    }
}
