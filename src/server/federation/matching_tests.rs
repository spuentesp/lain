use crate::federation::matching::*;
use crate::schema::{GraphNode, NodeType};

fn node(repo: &str, name: &str, sig: &str) -> GraphNode {
    let mut n = GraphNode::new(NodeType::Function, name.into(), "src/lib.rs".into());
    n.id = format!("{repo}:Function:src/lib.rs:{name}");
    n.signature = Some(sig.into());
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
    let new_node = node("repo1", "verify_token", "fn verify_token(user: &User) -> Result<Token>");
    let candidates = vec![
        node("repo2", "verify_token", "fn verify_token(u: &User) -> Result<Token>"),
        node("repo3", "validate", "fn validate(x: i32) -> bool"),
        node("repo4", "verify_token", "fn totally_different() -> String"),
    ];
    let matches = find_cross_repo_matches(&new_node, &candidates, 5, 0.5);
    let matched_ids: Vec<&str> = matches.iter().map(|(id, _)| id.as_str()).collect();
    assert!(matched_ids.contains(&"repo2:Function:src/lib.rs:verify_token"));
    assert!(!matched_ids.contains(&"repo3:Function:src/lib.rs:validate"));
    assert!(!matched_ids.contains(&"repo4:Function:src/lib.rs:verify_token"));
}

#[test]
fn find_cross_repo_matches_caps_at_top_k() {
    let new_node = node("repo1", "f", "fn f(x: i32)");
    let candidates: Vec<GraphNode> = (0..20).map(|i| {
        let mut n = node(&format!("repo{i}"), "f", "fn f(x: i32)");
        n.signature = Some("fn f(x: i32)".into());
        n
    }).collect();
    let matches = find_cross_repo_matches(&new_node, &candidates, 5, 0.0);
    assert_eq!(matches.len(), 5);
}

#[test]
fn find_cross_repo_matches_excludes_same_repo() {
    let new_node = node("repo1", "f", "fn f(x: i32)");
    let candidates = vec![node("repo1", "f", "fn f(x: i32)")];
    let matches = find_cross_repo_matches(&new_node, &candidates, 5, 0.0);
    assert!(matches.is_empty(), "same-repo matches should be excluded");
}
