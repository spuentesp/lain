//! Kani proofs for the civil-date arithmetic. Run: `cargo kani --harness <name>`.
use super::*;

/// Last day of year 9999 (1970-01-01 = day 0). Unbounded 64-bit division is
/// intractable for the SAT backend; every date the formatter can meaningfully
/// print (1970..=9999) is covered.
const MAX_DAYS: i64 = 2_932_896;

/// Every day in 1970..=9999 maps to a real calendar month and day-of-month,
/// with no arithmetic overflow or underflow.
#[kani::proof]
fn civil_from_days_is_total_with_valid_ranges() {
    let days: i64 = kani::any();
    kani::assume((0..=MAX_DAYS).contains(&days));
    let (_, m, d) = civil_from_days(days);
    assert!((1..=12).contains(&m));
    assert!((1..=31).contains(&d));
}

/// Consecutive days are consecutive dates: either the next day of the same
/// month, or the 1st of the following month (or year).
#[kani::proof]
fn civil_from_days_is_a_day_successor() {
    let days: i64 = kani::any();
    kani::assume((0..MAX_DAYS).contains(&days));
    let (y0, m0, d0) = civil_from_days(days);
    let (y1, m1, d1) = civil_from_days(days + 1);
    let same_month = y1 == y0 && m1 == m0 && d1 == d0 + 1;
    let next_month = y1 == y0 && m1 == m0 + 1 && d1 == 1;
    let next_year = y1 == y0 + 1 && m0 == 12 && m1 == 1 && d1 == 1;
    assert!(same_month || next_month || next_year);
}

/// Anchors: the epoch and a leap day.
#[kani::proof]
fn civil_from_days_known_dates() {
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(civil_from_days(11_016), (2000, 2, 29)); // 2000 is a leap year
    assert_eq!(civil_from_days(19_723), (2024, 1, 1));
}
