//! `needs_embedding` and the thread rule.
use super::*;
use proptest::prelude::*;

#[test]
fn missing_garbage_and_all_zero_embeddings_need_embedding() {
    assert!(needs_embedding(None));
    assert!(needs_embedding(Some("")));
    assert!(needs_embedding(Some("not json")));
    assert!(needs_embedding(Some("[]")));
    assert!(needs_embedding(Some("[0.0, 0.0, 0.0]")));
    assert!(needs_embedding(Some("[-0.0, 0]")));
    assert!(!needs_embedding(Some("[0.0, 0.5]")));
}

proptest! {
    #[test]
    fn a_vector_with_any_nonzero_component_is_a_real_embedding(
        v in prop::collection::vec(-1.0f32..1.0, 1..32), idx in 0usize..32
    ) {
        let mut v = v;
        let i = idx % v.len();
        v[i] = 0.25; // guarantee one nonzero component
        let json = serde_json::to_string(&v).unwrap();
        prop_assert!(!needs_embedding(Some(&json)));
    }

    #[test]
    fn an_all_zero_vector_of_any_length_needs_embedding(n in 0usize..64) {
        let json = serde_json::to_string(&vec![0.0f32; n]).unwrap();
        prop_assert!(needs_embedding(Some(&json)));
    }

    #[test]
    fn thread_rule_matches_its_contract(max in 0usize..64, cores in 0usize..64) {
        let n = intra_threads_for(max, cores);
        prop_assert!(n >= 1);
        if max == 0 { prop_assert!((1..=4).contains(&n)); } else { prop_assert_eq!(n, max); }
    }
}

#[test]
fn the_live_rule_never_returns_zero() {
    assert!(resolve_intra_threads(0) >= 1);
    assert_eq!(resolve_intra_threads(7), 7);
}
