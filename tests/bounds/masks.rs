//! `oracle::Mask`: one bit per row, counted over row ranges

use crate::oracle::Mask;

/// A fixed, irregular set of rows: crosses word boundaries, includes the
/// first and last bit of words, and leaves whole words empty
fn rows(total: u64) -> Vec<u64> {
    let mut rows: Vec<u64> = (0..total)
        .filter(|row| row % 7 == 0 || row % 64 == 63)
        .collect();
    rows.retain(|row| !(128..192).contains(row));
    rows
}

#[test]
fn count_matches_a_direct_count_over_every_range() {
    let total = 300;
    let set = rows(total);
    let mut mask = Mask::new(total);
    for &row in &set {
        mask.set(row);
    }
    for start in 0..=total {
        for end in start..=total {
            let expected = set
                .iter()
                .filter(|&&row| (start..end).contains(&row))
                .count();
            assert_eq!(
                mask.count(start, end),
                u64::try_from(expected).expect("small count"),
                "rows {start}..{end}"
            );
            assert_eq!(mask.any(start, end), expected > 0, "rows {start}..{end}");
        }
    }
}

#[test]
fn whole_words_count_all_64_bits() {
    let mut mask = Mask::new(256);
    for row in 64..192 {
        mask.set(row);
    }
    assert_eq!(mask.count(0, 256), 128);
    assert_eq!(mask.count(64, 128), 64);
    assert_eq!(mask.count(63, 193), 128);
}

#[test]
fn rows_past_the_end_are_ignored() {
    let mut mask = Mask::new(10);
    mask.set(9);
    mask.set(64);
    mask.set(u64::MAX);
    assert_eq!(mask.count(0, 10), 1);
    assert_eq!(mask.count(0, 1000), 1);
}

#[test]
fn empty_ranges_count_nothing() {
    let mut mask = Mask::new(64);
    mask.set(5);
    assert_eq!(mask.count(5, 5), 0);
    assert!(!mask.any(6, 6));
}
