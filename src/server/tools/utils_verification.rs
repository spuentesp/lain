use super::*;
use proptest::prelude::*;

proptest! {
    /// Total over all of i64, never renders a sign, and picks the unit the
    /// doc promises (< 1 min s, < 1 h m, < 1 d h, else d).
    #[test]
    fn format_duration_is_total_and_unsigned(secs in any::<i64>()) {
        let out = format_duration(secs);
        prop_assert!(!out.contains('-'), "{out}");
        let a = secs.unsigned_abs();
        let unit = match a { 0..=59 => 's', 60..=3599 => 'm', 3600..=86399 => 'h', _ => 'd' };
        prop_assert!(out.ends_with(&format!("{unit} ago")), "{out}");
    }

    /// Past and future of the same magnitude read identically.
    #[test]
    fn format_duration_is_symmetric(secs in 1i64..i64::MAX) {
        prop_assert_eq!(format_duration(secs), format_duration(-secs));
    }
}
