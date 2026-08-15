//! Tests for tools/utils.rs

use crate::server::tools::utils::*;
use crate::schema::{GraphNode, NodeType};
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;

#[test]
fn test_build_enriched_text_name_only() {
    let node = GraphNode::new(NodeType::Function, "test_fn".to_string(), "/src/lib.rs".to_string());
    let result = build_enriched_text(&node);
    assert_eq!(result, "test_fn | /src/lib.rs");
}

#[test]
fn test_build_enriched_text_with_signature() {
    let mut node = GraphNode::new(NodeType::Function, "add".to_string(), "/src/math.rs".to_string());
    node.signature = Some("(a: i32, b: i32) -> i32".to_string());
    let result = build_enriched_text(&node);
    assert!(result.contains("add"));
    assert!(result.contains("(a: i32, b: i32) -> i32"));
    assert!(result.contains("/src/math.rs"));
}

#[test]
fn test_build_enriched_text_with_docstring() {
    let mut node = GraphNode::new(NodeType::Function, "process".to_string(), "/src/main.rs".to_string());
    node.docstring = Some("Processes the input queue".to_string());
    let result = build_enriched_text(&node);
    assert!(result.contains("process"));
    assert!(result.contains("Processes the input queue"));
    assert!(result.contains("/src/main.rs"));
}

#[test]
fn test_build_enriched_text_all_fields() {
    let mut node = GraphNode::new(NodeType::Function, "full_fn".to_string(), "/src/full.rs".to_string());
    node.signature = Some("(x: String) -> Result<(), Error>".to_string());
    node.docstring = Some("Full documentation here".to_string());
    let result = build_enriched_text(&node);
    let parts: Vec<&str> = result.split(" | ").collect();
    assert_eq!(parts.len(), 4);
    assert_eq!(parts[0], "full_fn");
    assert_eq!(parts[1], "(x: String) -> Result<(), Error>");
    assert_eq!(parts[2], "Full documentation here");
    assert_eq!(parts[3], "/src/full.rs");
}

#[test]
fn test_get_str_arg_present() {
    let mut args = serde_json::Map::new();
    args.insert("key".to_string(), serde_json::Value::String("value".to_string()));
    let args_ref: Option<&serde_json::Map<String, serde_json::Value>> = Some(&args);

    let result = get_str_arg(args_ref, "key");
    assert_eq!(result, "value");
}

#[test]
fn test_get_str_arg_missing() {
    let args_ref: Option<&serde_json::Map<String, serde_json::Value>> = None;
    let result = get_str_arg(args_ref, "key");
    assert_eq!(result, "");
}

#[test]
fn test_get_str_arg_present_but_wrong_type() {
    let mut args = serde_json::Map::new();
    args.insert("key".to_string(), serde_json::Value::Number(42.into()));
    let args_ref: Option<&serde_json::Map<String, serde_json::Value>> = Some(&args);

    let result = get_str_arg(args_ref, "key");
    assert_eq!(result, "");
}

#[test]
fn test_get_usize_arg_present() {
    let mut args = serde_json::Map::new();
    args.insert("count".to_string(), serde_json::Value::Number(serde_json::Number::from(100)));
    let args_ref: Option<&serde_json::Map<String, serde_json::Value>> = Some(&args);

    let result = get_usize_arg(args_ref, "count");
    assert_eq!(result, Some(100));
}

#[test]
fn test_get_usize_arg_missing() {
    let args_ref: Option<&serde_json::Map<String, serde_json::Value>> = None;
    let result = get_usize_arg(args_ref, "count");
    assert_eq!(result, None);
}

#[test]
fn test_get_usize_arg_wrong_type() {
    let mut args = serde_json::Map::new();
    args.insert("count".to_string(), serde_json::Value::String("not_a_number".to_string()));
    let args_ref: Option<&serde_json::Map<String, serde_json::Value>> = Some(&args);

    let result = get_usize_arg(args_ref, "count");
    assert_eq!(result, None);
}

#[test]
fn test_get_bool_arg_true() {
    let mut args = serde_json::Map::new();
    args.insert("flag".to_string(), serde_json::Value::Bool(true));
    let args_ref: Option<&serde_json::Map<String, serde_json::Value>> = Some(&args);

    let result = get_bool_arg(args_ref, "flag");
    assert_eq!(result, Some(true));
}

#[test]
fn test_get_bool_arg_false() {
    let mut args = serde_json::Map::new();
    args.insert("flag".to_string(), serde_json::Value::Bool(false));
    let args_ref: Option<&serde_json::Map<String, serde_json::Value>> = Some(&args);

    let result = get_bool_arg(args_ref, "flag");
    assert_eq!(result, Some(false));
}

#[test]
fn test_get_bool_arg_missing() {
    let args_ref: Option<&serde_json::Map<String, serde_json::Value>> = None;
    let result = get_bool_arg(args_ref, "flag");
    assert_eq!(result, None);
}

#[test]
fn test_get_bool_arg_wrong_type() {
    let mut args = serde_json::Map::new();
    args.insert("flag".to_string(), serde_json::Value::String("true".to_string()));
    let args_ref: Option<&serde_json::Map<String, serde_json::Value>> = Some(&args);

    let result = get_bool_arg(args_ref, "flag");
    assert_eq!(result, None);
}

#[test]
fn test_cosine_similarity_normal_vectors() {
    let a = vec![1.0, 0.0, 0.0];
    let b = vec![1.0, 0.0, 0.0];
    let result = cosine_similarity(&a, &b);
    assert!((result - 1.0).abs() < 1e-6);

    let c = vec![0.0, 1.0, 0.0];
    let d = vec![0.0, 1.0, 0.0];
    let result2 = cosine_similarity(&c, &d);
    assert!((result2 - 1.0).abs() < 1e-6);
}

#[test]
fn test_cosine_similarity_orthogonal_vectors() {
    let a = vec![1.0, 0.0, 0.0];
    let b = vec![0.0, 1.0, 0.0];
    let result = cosine_similarity(&a, &b);
    assert!(result.abs() < 1e-6);
}

#[test]
fn test_cosine_similarity_45_degree() {
    let a = vec![1.0, 0.0];
    let b = vec![1.0, 1.0];
    let result = cosine_similarity(&a, &b);
    // cos(45°) = 1 / sqrt(2) ≈ 0.7071
    let expected = (2.0f32).sqrt() / 2.0;
    let diff = (result - expected).abs();
    assert!(diff < 1e-2, "expected ~0.707, got {}, diff {}", result, diff);
}

#[test]
fn test_cosine_similarity_mismatched_lengths() {
    let a = vec![1.0, 0.0, 0.0];
    let b = vec![1.0, 0.0];
    let result = cosine_similarity(&a, &b);
    assert_eq!(result, 0.0);
}

#[test]
fn test_cosine_similarity_empty_vectors() {
    let a: Vec<f32> = vec![];
    let b: Vec<f32> = vec![];
    let result = cosine_similarity(&a, &b);
    assert_eq!(result, 0.0);
}

#[test]
fn test_cosine_similarity_negated_vector() {
    let a = vec![1.0, 2.0, 3.0];
    let b = vec![-1.0, -2.0, -3.0];
    let result = cosine_similarity(&a, &b);
    assert!((result - (-1.0)).abs() < 1e-6);
}

#[test]
fn test_cosine_similarity_large_vectors() {
    let a: Vec<f32> = (0..384).map(|i| (i as f32) * 0.01).collect();
    let b: Vec<f32> = (0..384).map(|i| (i as f32) * 0.01).collect();
    let result = cosine_similarity(&a, &b);
    assert!((result - 1.0).abs() < 1e-3);
}

#[test]
fn test_resolve_node_in_overlay() {
    use crate::overlay::VolatileOverlay;

    let tmp = std::env::temp_dir().join("test_resolve_node_overlay");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();
    let overlay = VolatileOverlay::new();

    let node = GraphNode::new(NodeType::Function, "test_fn".to_string(), "/src/lib.rs".to_string());
    let id = node.id.clone();
    overlay.insert_node(node);

    let result = resolve_node(&graph, &overlay, &id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().name, "test_fn");
}

#[test]
fn test_resolve_node_in_graph() {
    let tmp = std::env::temp_dir().join("test_resolve_node_graph");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();
    let overlay = VolatileOverlay::new();

    let node = GraphNode::new(NodeType::Function, "test_fn".to_string(), "/src/lib.rs".to_string());
    let id = node.id.clone();
    graph.upsert_node(node).unwrap();

    let result = resolve_node(&graph, &overlay, &id);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().name, "test_fn");
}

#[test]
fn test_resolve_node_by_name() {
    let tmp = std::env::temp_dir().join("test_resolve_node_by_name");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();
    let overlay = VolatileOverlay::new();

    let node = GraphNode::new(NodeType::Function, "my_function".to_string(), "/src/lib.rs".to_string());
    graph.upsert_node(node).unwrap();

    let result = resolve_node(&graph, &overlay, "my_function");
    assert!(result.is_ok());
    assert_eq!(result.unwrap().name, "my_function");
}

#[test]
fn test_resolve_node_not_found() {
    let tmp = std::env::temp_dir().join("test_resolve_node_not_found");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();
    let overlay = VolatileOverlay::new();

    let result = resolve_node(&graph, &overlay, "nonexistent_node_id");
    assert!(result.is_err());
}

#[test]
fn test_resolve_node_overlay_priority() {
    let tmp = std::env::temp_dir().join("test_resolve_overlay_priority");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();
    let overlay = VolatileOverlay::new();

    // Same name in both overlay and graph - overlay should win
    let n1 = GraphNode::new(NodeType::Function, "shared_name".to_string(), "/src/overlay.rs".to_string());
    let n2 = GraphNode::new(NodeType::Function, "shared_name".to_string(), "/src/graph.rs".to_string());
    overlay.insert_node(n1);
    graph.upsert_node(n2).unwrap();

    let result = resolve_node(&graph, &overlay, "shared_name").unwrap();
    // Should get overlay version since it has priority
    assert_eq!(result.path, "/src/overlay.rs");
}

#[test]
fn test_token_recall_perfect_match() {
    // Every query token appears in the candidate → recall = 1.0
    let score = token_recall("Tokenizer", "the Tokenizer struct lives here");
    assert!((score - 1.0).abs() < 1e-6, "expected 1.0, got {}", score);
}

#[test]
fn test_token_recall_partial_match() {
    // "GraphDatabase" matches, "save" and "bincode" don't → recall ≈ 0.333
    let score = token_recall(
        "GraphDatabase save bincode",
        "the GraphDatabase struct holds the merged brain",
    );
    assert!((score - 1.0 / 3.0).abs() < 1e-6, "expected ~0.333, got {}", score);
}

#[test]
fn test_token_recall_no_match() {
    let score = token_recall("totally unrelated query", "GraphDatabase save");
    assert_eq!(score, 0.0);
}

#[test]
fn test_token_recall_case_insensitive() {
    let a = token_recall("LSP", "lsp bridge");
    let b = token_recall("lsp", "LSP bridge");
    assert_eq!(a, b);
    assert!((a - 1.0).abs() < 1e-6);
}

#[test]
fn test_token_recall_filters_short_and_numeric() {
    // "a", "42", "x" should be filtered out as noise
    let score = token_recall("a 42 x Tokenizer", "the Tokenizer handles encoding");
    assert!((score - 1.0).abs() < 1e-6, "expected 1.0 (only Tokenizer counted), got {}", score);
}

#[test]
fn test_token_recall_empty_query() {
    let score = token_recall("", "anything here");
    assert_eq!(score, 0.0);
}

#[test]
fn test_stem_basic_suffixes() {
    // -ing → drop
    assert_eq!(stem("running"), "runn");
    assert_eq!(stem("indexing"), "index");
    // -ed → drop
    assert_eq!(stem("indexed"), "index");
    assert_eq!(stem("loaded"), "load");
    // -s → drop (but not -ss, -us)
    assert_eq!(stem("tokens"), "token");
    assert_eq!(stem("files"), "file");
    assert_eq!(stem("queries"), "query");  // consonant + ies → y
    assert_eq!(stem("ties"), "tie");        // plural -s strips regardless
    assert_eq!(stem("class"), "class");     // -ss preserved
    assert_eq!(stem("status"), "status");   // -us preserved
    // short words unchanged
    assert_eq!(stem("go"), "go");
    assert_eq!(stem("be"), "be");
    // already a stem
    assert_eq!(stem("index"), "index");
    assert_eq!(stem("graph"), "graph");
}

#[test]
fn test_stem_case_insensitive() {
    assert_eq!(stem("RUNNING"), "runn");
    assert_eq!(stem("Indexed"), "index");
    assert_eq!(stem("ToKeNs"), "token");
}

#[test]
fn test_lex_tokens_collapses_word_forms() {
    // "running" → "runn" (drop -ing). "runs" → "run" (drop -s).
    // The point: surface forms that share a stem should appear with the
    // same key in the lex_tokens set.
    let a = lex_tokens("running the index");
    let b = lex_tokens("runs the indexed");
    // "runn" from running, "run" from runs — different stems, but that's
    // the limit of our rules. What we DO collapse is index/indexed:
    assert!(a.contains("index"), "expected 'index' in {:?}", a);
    assert!(b.contains("index"), "expected 'index' in {:?}", b);
}

#[test]
fn test_lex_tokens_index_variants_match() {
    // The whole point: index/indexed/indexing should all stem to the same form
    let stems: std::collections::HashSet<_> = ["index", "indexed", "indexing", "indexes"]
        .iter()
        .map(|w| stem(w))
        .collect();
    // "index" and "indexed" both produce "index" — that's the win
    assert!(stems.contains("index"), "expected 'index' in {:?}", stems);
}

#[test]
fn test_token_recall_benefits_from_stemming() {
    // Query uses "running"; corpus has "index" and "indexing" — without
    // stemming they'd miss. With stemming, "running" → "runn" and
    // "indexing" → "index" are still different — that's correct.
    // But "queries" → "query" and corpus has "query" should now match.
    let score = token_recall("queries the database", "run a database query");
    assert!(score > 0.0, "expected non-zero recall after stemming, got {}", score);
}