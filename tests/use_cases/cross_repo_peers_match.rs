//! Proving test for `find_cross_repo_matches` (#16).
//!
//! The matcher used to require a populated `signature` field on
//! both sides. rust-analyzer's `documentSymbol` doesn't always
//! populate `detail` (which feeds `signature`), so a function whose
//! name matches across repos would NOT get a `CrossRepoSameSymbol`
//! edge even when the rest of the criteria match.
//!
//! The fix falls back to a name-only signal when `signature` is
//! empty: a single token equal to the function name. Two functions
//! with the same name in different repos now produce a match with
//! similarity 1.0 (above the 0.5 threshold).
//!
//! This test calls the matcher function directly rather than going
//! through `project_repo` (which has a separate rekey-bug that
//! emits the target with the raw candidate id and trips a backend
//! edge-insert failure — outside the scope of #16). The matcher
//! itself is the unit under test, and verifying its similarity
//! calculation directly is the precise fix-pin.

#[path = "../common/mod.rs"]
mod common;
use lain::federation::matching::find_cross_repo_matches;
use lain::schema::{EdgeType, GraphNode, NodeType};

#[test]
fn cross_repo_peers_match_by_name_when_signature_missing() {
    // Two peer functions in different repos. Empty signature is the
    // bug condition: rust-analyzer didn't populate `detail`, so the
    // signature field is None.
    let mut new_node = GraphNode::new(NodeType::Function, "shared_helper".into(), "src/lib.rs".into());
    new_node.id = "11111111-1111-1111-1111-111111111111:Function:src/lib.rs:shared_helper".into();
    let mut candidate = GraphNode::new(NodeType::Function, "shared_helper".into(), "src/lib.rs".into());
    candidate.id = "22222222-2222-2222-2222-222222222222:Function:src/lib.rs:shared_helper".into();

    // Pre-fix: empty signature → empty tokens → similarity 0.0 → no
    // match. Post-fix: empty signature → single-token fallback
    // (`[name.to_lowercase()]`) → both sides have [shared_helper]
    // → similarity 1.0 → match.
    let matches = find_cross_repo_matches(&new_node, std::slice::from_ref(&candidate), 5, 0.5);

    assert_eq!(
        matches.len(),
        1,
        "expected exactly one match for name-only fallback; got {matches:?}"
    );
    let (target_gid, similarity) = &matches[0];
    assert!(
        target_gid.contains("22222222-2222-2222-2222-222222222222"),
        "matched target should be the candidate's id; got {target_gid}"
    );
    assert!(
        (similarity - 1.0).abs() < f32::EPSILON,
        "name-only fallback should produce similarity 1.0; got {similarity}"
    );

    // Sanity: functions with different names don't match.
    let mut other_named = GraphNode::new(NodeType::Function, "different_name".into(), "src/lib.rs".into());
    other_named.id = "33333333-3333-3333-3333-333333333333:Function:src/lib.rs:different_name".into();
    let no_match = find_cross_repo_matches(&new_node, std::slice::from_ref(&other_named), 5, 0.5);
    assert!(
        no_match.is_empty(),
        "different names should not match; got {no_match:?}"
    );

    // Sanity: with a real signature, the original signature-similarity
    // path is still used. Two functions with the same signature in
    // different repos should also match.
    let mut sig_a = GraphNode::new(NodeType::Function, "fn_with_sig".into(), "src/lib.rs".into());
    sig_a.id = "44444444-4444-4444-4444-444444444444:Function:src/lib.rs:fn_with_sig".into();
    sig_a.signature = Some("fn(a: u32) -> u32".into());
    let mut sig_b = GraphNode::new(NodeType::Function, "fn_with_sig".into(), "src/lib.rs".into());
    sig_b.id = "55555555-5555-5555-5555-555555555555:Function:src/lib.rs:fn_with_sig".into();
    sig_b.signature = Some("fn(a: u32) -> u32".into());
    let sig_match = find_cross_repo_matches(&sig_a, std::slice::from_ref(&sig_b), 5, 0.5);
    assert_eq!(
        sig_match.len(),
        1,
        "identical signatures should match; got {sig_match:?}"
    );

    // Suppress unused-import warning for EdgeType — kept for
    // downstream expansion to a project_repo integration test.
    let _ = EdgeType::Contains;
}
