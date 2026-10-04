//! `glob_rec` against its original recursive definition (the oracle), plus a
//! regression for the exponential-time blowup.
use super::*;
use proptest::prelude::*;

/// The pre-fix definition, kept verbatim as the specification.
fn oracle(p: &[u8], s: &[u8]) -> bool {
    if p.is_empty() {
        return s.is_empty();
    }
    if p[0] == b'*' {
        for i in 0..=s.len() {
            if i > 0 && s[i - 1] == b'/' {
                break;
            }
            if oracle(&p[1..], &s[i..]) {
                return true;
            }
        }
        return false;
    }
    if s.is_empty() {
        return false;
    }
    p[0] == s[0] && oracle(&p[1..], &s[1..])
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 4000, ..ProptestConfig::default() })]

    #[test]
    fn iterative_glob_equals_the_recursive_definition(
        p in "[ab/*]{0,9}", s in "[ab/]{0,10}"
    ) {
        prop_assert_eq!(glob_rec(p.as_bytes(), s.as_bytes()), oracle(p.as_bytes(), s.as_bytes()),
            "pattern {:?} text {:?}", p, s);
    }
}

#[test]
fn many_stars_against_a_long_segment_terminate_quickly() {
    let pattern = format!("{}b", "*a".repeat(14));
    let text = "a".repeat(250); // never matches: no `b`
    let started = std::time::Instant::now();
    assert!(!glob_rec(pattern.as_bytes(), text.as_bytes()));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "glob matching is not polynomial: {:?}",
        started.elapsed()
    );
}
