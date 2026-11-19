//! `stats`: when min/max statistics prove a filter matches no row
//!
//! A wrong "empty" makes a bound too good, the one mistake the bounds must
//! never make, so most cases here check both sides of each boundary.

use std::collections::HashMap;

use datafusion::arrow::datatypes::{DataType, Field};
use datafusion::logical_expr::Expr;
use datafusion::parquet::basic::Type as Physical;
use datafusion::prelude::{col, lit};
use datafusion::scalar::ScalarValue;

use crate::stats::{Ctx, Kind, Range, Val, proves_empty, usable};

/// Integer column `x` with values bounded by `[min, max]` and no nulls
fn ints(min: i128, max: i128) -> Range {
    Range {
        min: Some(Val::Int(min)),
        max: Some(Val::Int(max)),
        nulls: Some(0),
        rows: 100,
    }
}

/// Whether `expr` is proven empty with `range` as column `x`'s statistics
fn empty_with(expr: &Expr, kind: Kind, range: &Range) -> bool {
    let kinds = HashMap::from([("x".to_string(), kind)]);
    let lookup = |column: &str| (column == "x").then(|| range.clone());
    proves_empty(
        expr,
        &Ctx {
            kinds: &kinds,
            range: &lookup,
        },
    )
}

fn empty(expr: &Expr, range: &Range) -> bool {
    empty_with(expr, Kind::Int { scale: 0 }, range)
}

#[test]
fn comparisons_are_empty_only_past_the_range() {
    let r = ints(10, 20);
    assert!(empty(&col("x").lt(lit(10)), &r));
    assert!(!empty(&col("x").lt(lit(11)), &r));
    assert!(empty(&col("x").lt_eq(lit(9)), &r));
    assert!(!empty(&col("x").lt_eq(lit(10)), &r));
    assert!(empty(&col("x").gt(lit(20)), &r));
    assert!(!empty(&col("x").gt(lit(19)), &r));
    assert!(empty(&col("x").gt_eq(lit(21)), &r));
    assert!(!empty(&col("x").gt_eq(lit(20)), &r));
    assert!(empty(&col("x").eq(lit(9)), &r));
    assert!(empty(&col("x").eq(lit(21)), &r));
    assert!(!empty(&col("x").eq(lit(10)), &r));
    assert!(!empty(&col("x").eq(lit(20)), &r));
}

#[test]
fn literal_on_the_left_flips_the_operator() {
    let r = ints(10, 20);
    // 10 > x is x < 10
    assert!(empty(&lit(10).gt(col("x")), &r));
    assert!(!empty(&lit(11).gt(col("x")), &r));
    // 20 <= x is x >= 20
    assert!(!empty(&lit(20).lt_eq(col("x")), &r));
    assert!(empty(&lit(21).lt_eq(col("x")), &r));
}

#[test]
fn not_equal_is_empty_only_when_the_bounds_meet_at_the_value() {
    assert!(empty(&col("x").not_eq(lit(7)), &ints(7, 7)));
    assert!(!empty(&col("x").not_eq(lit(8)), &ints(7, 7)));
    assert!(!empty(&col("x").not_eq(lit(7)), &ints(7, 8)));
}

#[test]
fn between_and_in_list_need_every_value_outside() {
    let r = ints(10, 20);
    assert!(empty(&col("x").between(lit(21), lit(30)), &r));
    assert!(empty(&col("x").between(lit(0), lit(9)), &r));
    assert!(!empty(&col("x").between(lit(0), lit(10)), &r));
    assert!(!empty(&col("x").not_between(lit(21), lit(30)), &r));
    assert!(empty(&col("x").in_list(vec![lit(1), lit(30)], false), &r));
    assert!(!empty(&col("x").in_list(vec![lit(1), lit(15)], false), &r));
    assert!(!empty(&col("x").in_list(vec![lit(1), lit(30)], true), &r));
}

#[test]
fn and_needs_one_side_empty_or_needs_both() {
    let r = ints(10, 20);
    let out = col("x").gt(lit(50));
    let inside = col("x").gt(lit(15));
    assert!(empty(&out.clone().and(inside.clone()), &r));
    assert!(!empty(&out.clone().or(inside), &r));
    assert!(empty(&out.or(col("x").lt(lit(0))), &r));
}

#[test]
fn nulls_decide_is_null_and_every_comparison() {
    let all_null = Range {
        nulls: Some(100),
        rows: 100,
        ..Range::default()
    };
    assert!(empty(&col("x").gt(lit(0)), &all_null));
    assert!(empty(&col("x").is_not_null(), &all_null));
    assert!(!empty(&col("x").is_null(), &all_null));

    assert!(empty(&col("x").is_null(), &ints(1, 2)));
    let unknown_nulls = Range {
        nulls: None,
        ..ints(1, 2)
    };
    assert!(!empty(&col("x").is_null(), &unknown_nulls));
}

#[test]
fn missing_statistics_never_prove_empty() {
    assert!(!empty(&col("x").gt(lit(5)), &Range::default()));
    // A column with no statistics at all
    assert!(!empty(&col("y").gt(lit(5)), &ints(0, 1)));
    // Statistics of a kind that is never compared
    assert!(!empty_with(&col("x").gt(lit(5)), Kind::Other, &ints(0, 1)));
}

#[test]
fn decimal_literals_compare_at_the_column_scale() {
    let scale2 = Kind::Int { scale: 2 };
    // Unscaled at scale 2: 10.00 to 20.00
    let r = ints(1000, 2000);
    let dec = |unscaled: i128, scale: i8| lit(ScalarValue::Decimal128(Some(unscaled), 10, scale));
    assert!(empty_with(&col("x").gt(lit(20)), scale2, &r));
    assert!(!empty_with(&col("x").gt_eq(lit(20)), scale2, &r));
    // 20.005 has more digits than the column holds: not an exact value, kept
    assert!(!empty_with(&col("x").gt(dec(20005, 3)), scale2, &r));
    // 20.010 at scale 3 is 20.01 at scale 2
    assert!(empty_with(&col("x").gt_eq(dec(20010, 3)), scale2, &r));
    assert!(empty_with(&col("x").lt(dec(10, 0)), scale2, &r));
    assert!(!empty_with(&col("x").lt(dec(1001, 2)), scale2, &r));
}

#[test]
fn strings_compare_as_bytes_and_truncated_bounds_stay_bounds() {
    let r = Range {
        min: Some(Val::Bytes(b"b".to_vec())),
        max: Some(Val::Bytes(b"d".to_vec())),
        nulls: Some(0),
        rows: 10,
    };
    assert!(empty_with(&col("x").eq(lit("a")), Kind::Bytes, &r));
    assert!(empty_with(&col("x").eq(lit("da")), Kind::Bytes, &r));
    assert!(!empty_with(&col("x").eq(lit("c")), Kind::Bytes, &r));
    assert!(!empty_with(&col("x").eq(lit("d")), Kind::Bytes, &r));
    // Bounds that meet pin every value, even bounds that may be truncated
    let one = Range {
        max: Some(Val::Bytes(b"b".to_vec())),
        ..r
    };
    assert!(empty_with(&col("x").not_eq(lit("b")), Kind::Bytes, &one));
    assert!(!empty_with(&col("x").not_eq(lit("c")), Kind::Bytes, &one));
}

#[test]
fn kinds_leave_out_types_whose_order_differs_from_their_storage() {
    let kind =
        |data_type: DataType, physical| Kind::of(&Field::new("x", data_type, true), physical);
    assert_eq!(
        kind(DataType::Int32, Physical::INT32),
        Kind::Int { scale: 0 }
    );
    assert_eq!(
        kind(DataType::Date32, Physical::INT32),
        Kind::Int { scale: 0 }
    );
    assert_eq!(
        kind(DataType::UInt16, Physical::INT32),
        Kind::Int { scale: 0 }
    );
    assert_eq!(
        kind(DataType::Decimal128(15, 2), Physical::INT64),
        Kind::Int { scale: 2 }
    );
    assert_eq!(kind(DataType::Utf8, Physical::BYTE_ARRAY), Kind::Bytes);
    // Unsigned 32- and 64-bit values sort differently from signed storage
    assert_eq!(kind(DataType::UInt32, Physical::INT32), Kind::Other);
    assert_eq!(kind(DataType::UInt64, Physical::INT64), Kind::Other);
    assert_eq!(kind(DataType::Float64, Physical::DOUBLE), Kind::Other);
    // Decimals stored as fixed-length bytes aren't compared
    assert_eq!(
        kind(DataType::Decimal128(38, 2), Physical::FIXED_LEN_BYTE_ARRAY),
        Kind::Other
    );
}

#[test]
fn usable_means_statistics_could_ever_rule_rows_out() {
    let kinds = HashMap::from([
        ("x".to_string(), Kind::Int { scale: 0 }),
        ("f".to_string(), Kind::Other),
    ]);
    assert!(usable(&col("x").gt(lit(1)), &kinds));
    assert!(!usable(&col("f").gt(lit(1)), &kinds));
    assert!(!usable(&col("x").like(lit("%a%")), &kinds));
    // A float literal is never an exact integer value
    assert!(!usable(&col("x").gt(lit(1.5)), &kinds));
    assert!(usable(
        &col("x").gt(lit(1)).and(col("f").gt(lit(1))),
        &kinds
    ));
    assert!(!usable(
        &col("x").gt(lit(1)).or(col("f").gt(lit(1))),
        &kinds
    ));
    assert!(usable(&col("f").is_null(), &kinds));
}

#[test]
fn values_are_known_when_bounds_meet_without_nulls_or_all_are_null() {
    assert!(ints(7, 7).values_known());
    assert!(!ints(7, 8).values_known());
    let one_null = Range {
        nulls: Some(1),
        ..ints(7, 7)
    };
    assert!(!one_null.values_known(), "the null's position isn't known");
    let unknown_nulls = Range {
        nulls: None,
        ..ints(7, 7)
    };
    assert!(!unknown_nulls.values_known());
    let all_null = Range {
        nulls: Some(100),
        rows: 100,
        ..Range::default()
    };
    assert!(all_null.values_known());
    assert!(!Range::default().values_known(), "no statistics, no rows");
}
