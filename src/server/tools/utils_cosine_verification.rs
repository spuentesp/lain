//! `cosine_similarity` over its whole domain. Scores feed `sort_by` with
//! `partial_cmp(..).unwrap_or(Equal)`, which is not a total order once a NaN
//! is present, and Rust's sort may panic on an inconsistent comparator. So the
//! function must return a finite value in [-1, 1] for ANY finite input, signed
//! and extreme values included (the older properties only used 0..=1e6).
use super::*;
use proptest::prelude::*;

fn component() -> impl Strategy<Value = f32> {
    prop_oneof![
        -1.0f32..=1.0,
        -1e6f32..=1e6,
        prop::num::f32::NORMAL | prop::num::f32::SUBNORMAL | prop::num::f32::ZERO,
        Just(f32::MAX),
        Just(f32::MIN),
        Just(f32::MIN_POSITIVE),
        Just(1e30),
        Just(-1e30),
        Just(1e-30),
    ]
}

fn vec_of(len: std::ops::Range<usize>) -> impl Strategy<Value = Vec<f32>> {
    prop::collection::vec(component(), len)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 4000, ..ProptestConfig::default() })]

    #[test]
    fn result_is_finite_and_within_unit_range(
        (a, b) in (1usize..40).prop_flat_map(|n| (vec_of(n..n + 1), vec_of(n..n + 1)))
    ) {
        let c = cosine_similarity(&a, &b);
        prop_assert!(c.is_finite(), "non-finite {c} for {a:?} · {b:?}");
        prop_assert!((-1.0..=1.0).contains(&c), "{c} outside [-1,1] for {a:?} · {b:?}");
    }

    #[test]
    fn symmetric(
        (a, b) in (1usize..40).prop_flat_map(|n| (vec_of(n..n + 1), vec_of(n..n + 1)))
    ) {
        prop_assert_eq!(cosine_similarity(&a, &b), cosine_similarity(&b, &a));
    }

    #[test]
    fn a_nonzero_vector_is_maximally_similar_to_itself(a in vec_of(1..40)) {
        let c = cosine_similarity(&a, &a);
        // Zero (or underflowed) vectors have no direction: 0 by definition.
        prop_assert!(c == 0.0 || (c - 1.0).abs() < 1e-3, "self-similarity {c} for {a:?}");
    }
}
