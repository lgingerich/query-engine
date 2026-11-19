//! The whole bounds pipeline on the fixture: plan, metadata, pruning, data
//! pass, and DataFusion's own scan, with every number worked out by hand
//!
//! Each column holds 10 pages of 100 rows per row group (see `fixture`), so
//! "pages" below counts pages over all of a scan's columns.

use crate::fixture::{self, PAGE_ROWS, PageIndex, ROW_GROUP_ROWS, Session};
use crate::measure;
use crate::oracle::Mask;
use crate::scan::{self, DfScan};
use crate::stats::Kind;

/// Pages per column chunk
const PAGES: u64 = (ROW_GROUP_ROWS / PAGE_ROWS) as u64;
const ROW_GROUPS: usize = 4;

fn session(name: &str) -> Session {
    Session::new(&fixture::write(name, PageIndex::Yes))
}

#[test]
fn fixture_has_the_layout_the_expected_numbers_assume() {
    let s = session("layout");
    assert_eq!(s.file.num_row_groups(), ROW_GROUPS);
    for rg in 0..ROW_GROUPS {
        for leaf in 0..5 {
            let chunk = s.file.chunk(rg, leaf, Kind::Other);
            let pages = chunk.pages.expect("fixture has an offset index");
            let rows: Vec<u64> = pages.iter().map(|page| page.rows).collect();
            assert_eq!(rows, vec![100; 10], "row group {rg}, column {leaf}");
        }
    }
}

#[test]
fn id_range_keeps_one_row_group_and_the_two_pages_it_spans() {
    let s = session("id-range");
    let b = s.bounds("SELECT val FROM t WHERE id >= 2550 AND id < 2650");

    assert_eq!(b.columns, 2, "id for the filter, val for the output");
    assert_eq!(b.rows, 4000);
    assert!(b.filters >= 1, "the id range is pushed into the scan");
    assert_eq!(b.unusable, 0);
    assert_eq!((b.row_groups, b.row_groups_kept), (ROW_GROUPS, 1));
    // Rows 2550..2650 are in pages 2500..2600 and 2600..2700 of row group 2
    assert_eq!(b.pages, Some((2 * 4 * PAGES, 2 * 2)));
    assert_eq!(b.matching, Some(100));
    assert_eq!(b.reachable.values, 2 * 2 * 100);
    let late = b.late.expect("data pass ran");
    let oracle = b.oracle.expect("data pass ran");
    // Both pages of both columns hold matches: nothing left to skip
    assert_eq!((late.values, oracle.values), (400, 400));
    assert_eq!(late.bytes, b.reachable.bytes);
    assert_eq!(oracle.bytes, b.reachable.bytes);
    assert!(b.reachable.bytes < b.projection.bytes);
}

#[test]
fn late_materialization_reads_output_columns_only_where_rows_match() {
    let s = session("late");
    // `val = 92` holds only at id 1202; no `val` page can rule it out
    assert_eq!(fixture::val(1202), 92);
    let b = s.bounds("SELECT name FROM t WHERE id BETWEEN 1000 AND 1999 AND val = 92");

    assert_eq!(b.columns, 3);
    assert_eq!(b.row_groups_kept, 1);
    assert_eq!(b.pages, Some((3 * 4 * PAGES, 3 * PAGES)));
    assert_eq!(b.matching, Some(1));
    // Reachable: all 10 pages of row group 1, in each of the 3 columns
    assert_eq!(b.reachable.values, 3 * 1000);
    // Late: both filter columns as reachable, `name` only the page with row 1202
    assert_eq!(b.late.expect("data pass ran").values, 2 * 1000 + 100);
    // Oracle: one page in each column
    assert_eq!(b.oracle.expect("data pass ran").values, 3 * 100);
}

#[test]
fn no_filter_reads_every_chunk_of_the_needed_columns() {
    let s = session("no-filter");
    let b = s.bounds("SELECT name FROM t");

    let compressed: u64 = (0..ROW_GROUPS)
        .map(|rg| s.file.chunk(rg, 2, Kind::Bytes).compressed)
        .sum();
    assert_eq!(b.projection.bytes, compressed);
    assert_eq!(b.reachable.bytes, compressed);
    assert_eq!(b.late.map(|c| c.bytes), Some(compressed));
    assert_eq!(b.oracle.map(|c| c.bytes), Some(compressed));
    assert_eq!(b.row_groups_kept, ROW_GROUPS);
    assert_eq!(b.matching, Some(4000));
    assert_eq!(b.reachable.values, 4000);
}

#[test]
fn string_bounds_prune_sorted_strings() {
    let s = session("strings");
    let b = s.bounds("SELECT id FROM t WHERE name < 'n00150'");
    assert_eq!(b.row_groups_kept, 1);
    assert_eq!(b.matching, Some(150));
    // Pages 0..100 and 100..200 of `id` and `name`
    assert_eq!(b.pages.map(|(_, kept)| kept), Some(4));
}

#[test]
fn null_counts_prune_all_null_and_null_free_row_groups() {
    let s = session("nulls");
    let not_null = s.bounds("SELECT id FROM t WHERE price IS NOT NULL");
    assert_eq!(not_null.row_groups_kept, 3, "row group 3 is all null");
    assert_eq!(not_null.matching, Some(3000));

    let null = s.bounds("SELECT id FROM t WHERE price IS NULL");
    assert_eq!(null.row_groups_kept, 1, "row groups 0-2 have no nulls");
    assert_eq!(null.matching, Some(1000));
}

#[test]
fn decimal_filters_prune_at_the_column_scale() {
    let s = session("decimals");
    let b = s.bounds("SELECT id FROM t WHERE price >= CAST(2999.50 AS DECIMAL(10, 2))");
    assert_eq!(b.unusable, 0);
    assert_eq!(b.matching, Some(1), "only id 2999 costs 2999.50");
    // Row group 2, its last page; row group 3 is all null
    assert_eq!(b.row_groups_kept, 1);
    assert_eq!(b.pages.map(|(_, kept)| kept), Some(2));
}

#[test]
fn or_keeps_the_row_groups_of_either_side() {
    let s = session("or");
    let b = s.bounds("SELECT name FROM t WHERE id < 100 OR id >= 3900");
    assert_eq!(b.row_groups_kept, 2);
    assert_eq!(b.matching, Some(200));
    assert_eq!(b.pages.map(|(_, kept)| kept), Some(2 * 2));
}

#[test]
fn filters_statistics_cant_use_keep_every_row() {
    let s = session("unusable");
    let b = s.bounds("SELECT id FROM t WHERE name LIKE '%7%'");
    assert_eq!(b.unusable, 1);
    assert_eq!(b.row_groups_kept, ROW_GROUPS);
    assert_eq!(b.reachable.bytes, b.projection.bytes);
    // The data pass still finds every match
    let ids_with_7 = (0..4000)
        .filter(|id| format!("n{id:05}").contains('7'))
        .count();
    assert_eq!(
        b.matching,
        Some(u64::try_from(ids_with_7).expect("small count"))
    );
}

#[test]
fn without_a_page_index_whole_chunks_count() {
    let s = Session::new(&fixture::write("no-page-index", PageIndex::No));
    let b = s.bounds("SELECT val FROM t WHERE id >= 2550 AND id < 2650");
    assert_eq!(b.pages, None);
    assert_eq!(b.row_groups_kept, 1);
    let chunks: u64 = [0, 1]
        .iter()
        .map(|&leaf| s.file.chunk(2, leaf, Kind::Other).compressed)
        .sum();
    assert_eq!(b.reachable.bytes, chunks);
    assert_eq!(b.reachable.values, 2 * 1000);

    // Chunk statistics alone give every value of `k`: only `id` is read
    let known = s.bounds("SELECT k FROM t WHERE id >= 2550 AND id < 2650");
    let id_chunk = s.file.chunk(2, 0, Kind::Other).compressed;
    assert_eq!(known.reachable.bytes, id_chunk);
    assert_eq!(known.reachable.values, 1000);
}

#[test]
fn a_match_in_rows_statistics_ruled_out_is_reported_as_a_bug() {
    let s = session("pruning-bug");
    let scan = s.scan("SELECT val FROM t WHERE id >= 2550 AND id < 2650");
    // Pretend row 0 matched: statistics ruled out row group 0
    let mut mask = Mask::new(s.file.rows());
    mask.set(0);
    let Err(err) = measure::bound(&scan, &s.file, Some(&mask)) else {
        panic!("a match in a pruned row group must be an error");
    };
    assert!(err.contains("pruning bug"), "unexpected error: {err}");
}

#[test]
fn self_join_plans_one_scan_per_side_with_unqualified_filters() {
    let s = session("self-join");
    let scans = s.scans("SELECT a.name FROM t a JOIN t b ON a.id = b.val WHERE b.id < 100");
    assert_eq!(scans.len(), 2);
    let filtered: Vec<_> = scans
        .iter()
        .filter(|scan| !scan.filters.is_empty())
        .collect();
    assert_eq!(filtered.len(), 1, "only `b` has a filter");
    assert_eq!(
        filtered[0].filter_columns().into_iter().collect::<Vec<_>>(),
        vec!["id".to_string()]
    );
}

#[test]
fn datafusion_alone_prunes_what_the_bounds_prune_and_reads_no_less() {
    let s = session("datafusion");
    let sql = "SELECT val FROM t WHERE id >= 2550 AND id < 2650";
    let b = s.bounds(sql);
    let df = s.datafusion(sql);
    assert_eq!(df.row_groups_pruned, ROW_GROUPS - b.row_groups_kept);
    assert!(
        u64::try_from(df.bytes_scanned).expect("fits") >= b.reachable.bytes,
        "DataFusion read {} bytes, below the reachable bound {}",
        df.bytes_scanned,
        b.reachable.bytes
    );
}

#[test]
fn self_join_gives_each_scan_its_own_datafusion_numbers() {
    let s = session("self-join-datafusion");
    let scans = s.scans("SELECT a.name FROM t a JOIN t b ON a.id = b.val WHERE b.id < 100");
    let mut total = DfScan::default();
    for scan in &scans {
        let df = s
            .runtime
            .block_on(scan::datafusion_scan(&s.ctx, scan))
            .expect("scan runs alone");
        let expected = if scan.filters.is_empty() { 0 } else { 3 };
        assert_eq!(
            df.row_groups_pruned, expected,
            "filters: {:?}",
            scan.filters
        );
        total.add(df);
    }
    assert_eq!(total.row_groups_pruned, 3);
}

#[test]
fn run_counts_output_rows_and_final_aggregate_groups() {
    let s = session("run");
    let run = s
        .runtime
        .block_on(scan::run(
            &s.ctx,
            "SELECT id % 10, count(*) FROM t GROUP BY id % 10",
        ))
        .expect("query runs");
    assert_eq!(run.output_rows, 10);
    assert_eq!(run.groups, vec![Some(10)]);

    // With a LIMIT and no ORDER BY, DataFusion may stop aggregating early
    let limited = s
        .runtime
        .block_on(scan::run(
            &s.ctx,
            "SELECT val, count(*) FROM t GROUP BY val LIMIT 3",
        ))
        .expect("query runs");
    assert_eq!(limited.output_rows, 3);
    assert_eq!(limited.groups, vec![None]);

    // ORDER BY ... LIMIT sorts every group first, so the count is complete
    let sorted = s
        .runtime
        .block_on(scan::run(
            &s.ctx,
            "SELECT val, count(*) AS c FROM t GROUP BY val ORDER BY c DESC, val LIMIT 3",
        ))
        .expect("query runs");
    assert_eq!(sorted.groups, vec![Some(4000)]);
}

#[test]
fn only_statements_that_return_rows_are_queries() {
    assert!(scan::is_query("SELECT 1"));
    assert!(scan::is_query("  with x as (select 1) select * from x"));
    assert!(scan::is_query("(SELECT 1) UNION (SELECT 2)"));
    assert!(!scan::is_query("CREATE VIEW v AS SELECT 1"));
    assert!(!scan::is_query("drop view v"));
}

#[test]
fn chunks_whose_values_statistics_give_cost_nothing() {
    let s = session("known-values");
    // `k` is 7 everywhere; row group 3 of `price` is all null
    let b = s.bounds("SELECT k, price FROM t WHERE id >= 3500");
    let id_pages_bytes: u64 = s
        .file
        .chunk(3, 0, Kind::Int { scale: 0 })
        .pages
        .expect("page index")
        .iter()
        .skip(5)
        .map(|page| page.bytes)
        .sum();
    // Only the 5 `id` pages from row 3500 on are read
    assert_eq!(b.row_groups_kept, 1);
    assert_eq!(b.pages.map(|(_, kept)| kept), Some(5));
    assert_eq!(b.reachable.bytes, id_pages_bytes);
    assert_eq!(b.reachable.values, 500);
    assert_eq!(b.oracle.map(|c| c.values), Some(500));
    // Projection still reads every chunk whole: it uses no statistics
    let whole: u64 = (0..ROW_GROUPS)
        .flat_map(|rg| [0, 3, 4].map(|leaf| s.file.chunk(rg, leaf, Kind::Other).compressed))
        .sum();
    assert_eq!(b.projection.bytes, whole);
    assert_eq!(b.projection.values, 3 * 4000);

    // DataFusion reads nothing for `k` either, and no less than the bound
    let df = s.datafusion("SELECT k, price FROM t WHERE id >= 3500");
    assert!(u64::try_from(df.bytes_scanned).expect("fits") >= b.reachable.bytes);
}
