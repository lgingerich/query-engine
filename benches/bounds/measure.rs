//! The bounds for one table scan
//!
//! Per row group: the row segments statistics can't rule out (chunk min/max,
//! then page min/max where there is a page index), then the cost of reading
//! each needed column at each level:
//!
//! - projection: every chunk of every needed column, read whole: no
//!   statistics used
//! - reachable: pages (or chunks) that overlap a surviving segment
//! - late materialization: filter columns as reachable; other columns only
//!   pages (or chunks) holding a match. Reachable too, by reading the filter
//!   columns first
//! - oracle: every needed column only where it holds a match
//!
//! A chunk's dictionary page counts whenever any of its data pages does.
//! Below projection, a chunk or page whose statistics give every value (see
//! [`crate::stats::Range::values_known`]) costs nothing.

use std::collections::{BTreeSet, HashMap};

use datafusion::logical_expr::Expr;

use crate::file::{Chunk, ParquetFile};
use crate::oracle::Mask;
use crate::scan::Scan;
use crate::stats::{Ctx, Kind, proves_empty, usable};

/// Bytes and values for one level
#[derive(Default, Clone, Copy)]
pub(crate) struct Cost {
    pub(crate) bytes: u64,
    /// Uncompressed, by whole column chunk: a chunk counts in full when any
    /// of its pages does, because page headers alone give its size
    pub(crate) uncompressed: u64,
    /// Values decoded, by whole page
    pub(crate) values: u64,
}

impl Cost {
    fn add(&mut self, other: Self) {
        self.bytes += other.bytes;
        self.uncompressed += other.uncompressed;
        self.values += other.values;
    }
}

pub(crate) struct Bounds {
    pub(crate) columns: usize,
    pub(crate) filters: usize,
    /// Filters statistics can never use
    pub(crate) unusable: usize,
    pub(crate) rows: u64,
    pub(crate) row_groups: usize,
    pub(crate) row_groups_kept: usize,
    /// Data pages of the needed columns, and those reachable and needing a
    /// read, if every chunk has an offset index
    pub(crate) pages: Option<(u64, u64)>,
    /// Footer plus the page index of the needed chunks
    pub(crate) meta_bytes: u64,
    pub(crate) projection: Cost,
    pub(crate) reachable: Cost,
    /// `None` when the data pass couldn't run
    pub(crate) late: Option<Cost>,
    pub(crate) oracle: Option<Cost>,
    pub(crate) matching: Option<u64>,
}

/// Bounds for `scan`. `mask` is the data pass result: `None` when the scan has
/// filters and the pass failed. Errors if statistics ruled out a matching row,
/// which would mean a bug in the pruning logic
pub(crate) fn bound(
    scan: &Scan,
    file: &ParquetFile,
    mask: Option<&Mask>,
) -> Result<Bounds, String> {
    let kinds: HashMap<String, Kind> = scan
        .columns
        .iter()
        .map(|name| (name.clone(), file.kind(name, &scan.schema)))
        .collect();
    let leaves = scan
        .columns
        .iter()
        .map(|name| {
            file.leaf(name)
                .ok_or_else(|| format!("column `{name}` not in {}", scan.path.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let filter_columns = scan.filter_columns();
    let has_filters = !scan.filters.is_empty();
    // With no filters, every row matches: late materialization and the oracle
    // read what the reachable level reads
    let mask = if has_filters { mask } else { None };
    let data_pass = !has_filters || mask.is_some();

    let mut out = Bounds {
        columns: scan.columns.len(),
        filters: scan.filters.len(),
        unusable: scan.filters.iter().filter(|f| !usable(f, &kinds)).count(),
        rows: file.rows(),
        row_groups: file.num_row_groups(),
        row_groups_kept: 0,
        // Counted only when there are columns to read and all have an offset index
        pages: (!scan.columns.is_empty()).then_some((0, 0)),
        meta_bytes: file.footer_bytes,
        projection: Cost::default(),
        reachable: Cost::default(),
        late: data_pass.then(Cost::default),
        oracle: data_pass.then(Cost::default),
        matching: if has_filters {
            mask.map(|m| m.count(0, file.rows()))
        } else {
            Some(file.rows())
        },
    };

    let mut base = 0u64;
    for rg in 0..file.num_row_groups() {
        let rows = file.row_group_rows(rg);
        let chunks: Vec<Chunk> = scan
            .columns
            .iter()
            .zip(&leaves)
            .map(|(name, leaf)| file.chunk(rg, *leaf, kinds[name]))
            .collect();
        let by_name: HashMap<&str, &Chunk> = scan
            .columns
            .iter()
            .map(String::as_str)
            .zip(&chunks)
            .collect();
        let segments = kept_segments(&scan.filters, &filter_columns, &kinds, &by_name, rows);

        if let Some(mask) = mask {
            for (start, end) in gaps(&segments, rows) {
                if mask.any(base + start, base + end) {
                    return Err(format!(
                        "statistics ruled out matching rows {}..{} in row group {rg} of {}: a pruning bug",
                        base + start,
                        base + end,
                        scan.path.display()
                    ));
                }
            }
        }
        if !segments.is_empty() {
            out.row_groups_kept += 1;
        }

        let overlaps = |s: u64, e: u64| segments.iter().any(|&(a, b)| a < e && s < b);
        let holds_match = |s: u64, e: u64| mask.is_none_or(|m| m.any(base + s, base + e));
        for (name, chunk) in scan.columns.iter().zip(&chunks) {
            out.meta_bytes += chunk.index_bytes;
            out.projection.add(Cost {
                bytes: chunk.compressed,
                uncompressed: chunk.uncompressed,
                values: chunk.rows,
            });
            let (reachable, kept) = cost(chunk, overlaps);
            out.reachable.add(reachable);
            out.pages = match (out.pages, &chunk.pages) {
                (Some((total, reach)), Some(pages)) => Some((
                    total + u64::try_from(pages.len()).unwrap_or(0),
                    reach + kept,
                )),
                _ => None,
            };
            if let Some(late) = &mut out.late {
                if filter_columns.contains(name) {
                    late.add(reachable);
                } else {
                    late.add(cost(chunk, |s, e| overlaps(s, e) && holds_match(s, e)).0);
                }
            }
            if let Some(oracle) = &mut out.oracle {
                oracle.add(cost(chunk, holds_match).0);
            }
        }
        base += rows;
    }
    Ok(out)
}

/// The cost of reading the pages of `chunk` (or the whole chunk, without an
/// offset index) for which `keep(first_row, end_row)` holds, and how many
/// pages. Pages and chunks whose values the statistics give are never read
fn cost(chunk: &Chunk, keep: impl Fn(u64, u64) -> bool) -> (Cost, u64) {
    if chunk.range.values_known() {
        return (Cost::default(), 0);
    }
    match &chunk.pages {
        Some(pages) => {
            let mut out = Cost::default();
            let mut kept = 0;
            for page in pages {
                if !page.range.values_known() && keep(page.first_row, page.first_row + page.rows) {
                    out.bytes += page.bytes;
                    out.values += page.rows;
                    kept += 1;
                }
            }
            if kept > 0 {
                out.bytes += chunk.dictionary_bytes();
                out.uncompressed = chunk.uncompressed;
            }
            (out, kept)
        }
        None if keep(0, chunk.rows) => (
            Cost {
                bytes: chunk.compressed,
                uncompressed: chunk.uncompressed,
                values: chunk.rows,
            },
            0,
        ),
        None => (Cost::default(), 0),
    }
}

/// Row segments `[start, end)` of a row group that no filter can rule out
fn kept_segments(
    filters: &[Expr],
    filter_columns: &BTreeSet<String>,
    kinds: &HashMap<String, Kind>,
    chunks: &HashMap<&str, &Chunk>,
    rows: u64,
) -> Vec<(u64, u64)> {
    if filters.is_empty() || rows == 0 {
        return vec![(0, rows)];
    }
    let chunk_range = |column: &str| chunks.get(column).map(|chunk| chunk.range.clone());
    let ctx = Ctx {
        kinds,
        range: &chunk_range,
    };
    if filters.iter().any(|filter| proves_empty(filter, &ctx)) {
        return Vec::new();
    }

    // Split at every page boundary of every filter column, then test each
    // segment against the pages covering it
    let mut cuts: BTreeSet<u64> = [0, rows].into();
    for column in filter_columns {
        if let Some(pages) = chunks.get(column.as_str()).and_then(|c| c.pages.as_ref()) {
            cuts.extend(
                pages
                    .iter()
                    .map(|page| page.first_row)
                    .filter(|&r| r < rows),
            );
        }
    }
    let cuts: Vec<u64> = cuts.into_iter().collect();
    let mut kept: Vec<(u64, u64)> = Vec::new();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let page_range = |column: &str| {
            let chunk = chunks.get(column)?;
            Some(
                chunk
                    .page_at(start)
                    .map_or_else(|| chunk.range.clone(), |page| page.range.clone()),
            )
        };
        let page_ctx = Ctx {
            kinds,
            range: &page_range,
        };
        if filters.iter().any(|filter| proves_empty(filter, &page_ctx)) {
            continue;
        }
        match kept.last_mut() {
            Some(last) if last.1 == start => last.1 = end,
            _ => kept.push((start, end)),
        }
    }
    kept
}

/// The parts of `[0, rows)` outside `segments`
fn gaps(segments: &[(u64, u64)], rows: u64) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut at = 0;
    for &(start, end) in segments {
        if start > at {
            out.push((at, start));
        }
        at = end;
    }
    if at < rows {
        out.push((at, rows));
    }
    out
}
