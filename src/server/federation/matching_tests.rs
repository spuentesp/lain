use crate::federation::matching::*;
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::schema::{GraphNode, NodeType};

fn node(repo: &str, name: &str, sig: &str) -> GraphNode {
    let mut n = GraphNode::new(NodeType::Function, name.into(), "src/lib.rs".into());
    n.id = GlobalId::new(
        &RepoId::new(repo).unwrap(),
        NodeType::Function,
        "src/lib.rs",
        name,
        None,
    )
    .as_str()
    .to_string();
    n.signature = Some(sig.into());
    n
}

fn mk_node(name: &str, kind: &str, repo: &str, signature: Option<&str>, line: u32) -> GraphNode {
    let node_kind = match kind {
        "Function" => NodeType::Function,
        "Method" => NodeType::Method,
        _ => panic!("unsupported test kind {kind}"),
    };
    let mut n = GraphNode::new(node_kind.clone(), name.into(), "src/lib.rs".into())
        .with_location(line, line + 2);
    n.id = GlobalId::new(
        &RepoId::new(repo).unwrap(),
        node_kind,
        "src/lib.rs",
        name,
        Some(line),
    )
    .as_str()
    .to_string();
    n.signature = signature.map(|s| s.to_string());
    n
}

#[test]
fn signature_tokens_splits_on_punctuation() {
    let toks = signature_tokens("fn verify_token(user: &User) -> Result<Token>");
    assert!(toks.contains(&"verify_token".to_string()));
    assert!(toks.contains(&"user".to_string()));
    assert!(toks.contains(&"user".to_string())); // appears twice via "user:" and "&User"
    assert!(toks.contains(&"token".to_string()));
}

#[test]
fn signature_similarity_identical_is_one() {
    let a = signature_tokens("fn foo(x: i32) -> i32");
    let b = signature_tokens("fn foo(x: i32) -> i32");
    assert!((signature_similarity(&a, &b) - 1.0).abs() < 1e-6);
}

#[test]
fn signature_similarity_disjoint_is_zero() {
    let a = signature_tokens("fn alpha(x: i32)");
    let b = signature_tokens("fn beta(y: String)");
    assert_eq!(signature_similarity(&a, &b), 0.0);
}

#[test]
fn find_cross_repo_matches_above_threshold() {
    let new_node = node(
        "repo1",
        "verify_token",
        "fn verify_token(user: &User) -> Result<Token>",
    );
    let candidates = vec![
        node(
            "repo2",
            "verify_token",
            "fn verify_token(u: &User) -> Result<Token>",
        ),
        node("repo3", "validate", "fn validate(x: i32) -> bool"),
        node("repo4", "verify_token", "fn totally_different() -> String"),
    ];
    let matches = find_cross_repo_matches(&new_node, &candidates, 5, 0.5, false);
    let matched_ids: Vec<&str> = matches.iter().map(|(id, _, _)| id.as_str()).collect();
    assert!(matched_ids.contains(&"repo2:Function:src/lib.rs:verify_token:0"));
    assert!(!matched_ids.contains(&"repo3:Function:src/lib.rs:validate:0"));
    assert!(!matched_ids.contains(&"repo4:Function:src/lib.rs:verify_token:0"));
}

#[test]
fn find_cross_repo_matches_caps_at_top_k() {
    let new_node = node("repo1", "f", "fn f(x: i32)");
    let candidates: Vec<GraphNode> = (0..20)
        .map(|i| {
            let mut n = node(&format!("repo{i}"), "f", "fn f(x: i32)");
            n.signature = Some("fn f(x: i32)".into());
            n
        })
        .collect();
    let matches = find_cross_repo_matches(&new_node, &candidates, 5, 0.0, false);
    assert_eq!(matches.len(), 5);
}

#[test]
fn find_cross_repo_matches_excludes_same_repo() {
    let new_node = node("repo1", "f", "fn f(x: i32)");
    let candidates = vec![node("repo1", "f", "fn f(x: i32)")];
    let matches = find_cross_repo_matches(&new_node, &candidates, 5, 0.0, false);
    assert!(matches.is_empty(), "same-repo matches should be excluded");
}

#[test]
fn find_cross_repo_matches_both_signatures_empty_returns_empty() {
    let a = mk_node("a", "Method", "repo_a", None, 1);
    let b = mk_node("a", "Method", "repo_b", None, 1);
    let out = find_cross_repo_matches(&a, &[b], 5, 0.5, false);
    assert!(
        out.is_empty(),
        "empty signatures must produce zero matches, got {out:?}"
    );
}

#[test]
fn find_cross_repo_matches_name_only_without_flag_returns_empty() {
    // Name overlap exists but signatures are also empty → must refuse.
    let a = mk_node("verify_token", "Function", "repo_a", None, 1);
    let b = mk_node("verify_token", "Function", "repo_b", None, 1);
    let out = find_cross_repo_matches(&a, &[b], 5, 0.5, false);
    assert!(out.is_empty());
}

#[test]
fn find_cross_repo_matches_shared_param_name_passes() {
    let a = mk_node("foo", "Function", "repo_a", Some("pub fn foo(x: u32)"), 1);
    let b = mk_node("foo", "Function", "repo_b", Some("pub fn foo(x: i64)"), 1);
    let out = find_cross_repo_matches(&a, &[b], 5, 0.5, false);
    assert_eq!(out.len(), 1, "shared param `x` should match, got {out:?}");
    let (id, _sim, conf) = &out[0];
    assert!(id.starts_with("repo_b:"));
    assert_eq!(*conf, MatchConfidence::Signature);
}

#[test]
fn find_cross_repo_matches_stop_words_filtered() {
    // Both signatures are pure stop words (just `fn new()`). No
    // non-stop tokens to overlap. The `new` itself is in
    // SIGNATURE_STOP_WORDS, so it doesn't count as overlap.
    let a = mk_node("new", "Function", "repo_a", Some("pub fn new"), 1);
    let b = mk_node("new", "Function", "repo_b", Some("fn new"), 1);
    let out = find_cross_repo_matches(&a, &[b], 5, 0.5, false);
    assert!(
        out.is_empty(),
        "stop-word-only signatures should not match, got {out:?}"
    );
}
