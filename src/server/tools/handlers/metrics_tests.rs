//! Tests for tools/handlers/metrics.rs

use crate::nlp::NlpEmbedder;
use crate::server::presence::OccupancyMap;
use crate::server::tools::handlers::metrics::{find_anchors, get_anchor_score, get_context_depth, find_dead_code, explain_symbol, suggest_refactor_targets};
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::{GraphNode, NodeType, EdgeType, GraphEdge};
use std::sync::Arc;
use parking_lot::Mutex;
use std::collections::HashMap;

fn make_test_graph_with_nodes() -> (GraphDatabase, VolatileOverlay) {
    let tmp = std::env::temp_dir().join("test_metrics_graph");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    // Create a simple function graph: main -> a -> b
    let main = GraphNode::new(NodeType::Function, "main".to_string(), "/src/main.rs".to_string());
    let a = GraphNode::new(NodeType::Function, "a".to_string(), "/src/a.rs".to_string());
    let b = GraphNode::new(NodeType::Function, "b".to_string(), "/src/b.rs".to_string());

    graph.upsert_node(main.clone()).unwrap();
    graph.upsert_node(a.clone()).unwrap();
    graph.upsert_node(b.clone()).unwrap();

    graph.insert_edge(&GraphEdge::new(EdgeType::Calls, main.id.clone(), a.id.clone())).unwrap();
    graph.insert_edge(&GraphEdge::new(EdgeType::Calls, a.id.clone(), b.id.clone())).unwrap();

    let overlay = VolatileOverlay::new();
    (graph, overlay)
}

#[test]
fn test_find_anchors_basic() {
    let (graph, overlay) = make_test_graph_with_nodes();

    let result = find_anchors(&graph, &overlay, 5);
    assert!(result.is_ok());
    let text = result.unwrap();
    // May be empty if no anchor scores calculated, or show anchors if calculate_anchor_scores was run
    if !text.contains("No anchors") {
        assert!(text.contains("anchors") || text.contains("Top"));
    }
}

#[test]
fn test_get_anchor_score_existing() {
    let (graph, overlay) = make_test_graph_with_nodes();

    // Create node with anchor score in overlay
    let mut node = GraphNode::new(NodeType::Function, "test_fn".to_string(), "/src/lib.rs".to_string());
    node.anchor_score = Some(0.5);
    overlay.insert_node(node);

    let result = get_anchor_score(&graph, &overlay, "test_fn");
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("test_fn"));
}

#[test]
fn test_get_anchor_score_not_found() {
    let (graph, overlay) = make_test_graph_with_nodes();

    let result = get_anchor_score(&graph, &overlay, "nonexistent");
    // Returns error when node not found
    assert!(result.is_err());
}

#[test]
fn test_get_context_depth_existing() {
    let (graph, overlay) = make_test_graph_with_nodes();

    // Create node with depth in overlay
    let mut node = GraphNode::new(NodeType::Function, "deep_fn".to_string(), "/src/lib.rs".to_string());
    node.depth_from_main = Some(3);
    overlay.insert_node(node);

    let result = get_context_depth(&graph, &overlay, "deep_fn");
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("deep_fn"));
}

#[test]
fn test_get_context_depth_not_found() {
    let (graph, overlay) = make_test_graph_with_nodes();

    let result = get_context_depth(&graph, &overlay, "nonexistent");
    // Returns error when node not found
    assert!(result.is_err());
}

#[test]
fn test_find_dead_code() {
    let (graph, overlay) = make_test_graph_with_nodes();
    let embedder = NlpEmbedder::new_stub();
    let cache = Arc::new(Mutex::new(HashMap::new()));

    let result = find_dead_code(std::path::Path::new(""), &graph, &overlay, None, &embedder, &cache);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("dead code") || text.contains("Found"));
}

#[test]
fn test_explain_symbol_existing() {
    let (graph, overlay) = make_test_graph_with_nodes();

    // Put node in overlay with all fields
    let mut node = GraphNode::new(NodeType::Function, "documented_fn".to_string(), "/src/lib.rs".to_string());
    node.signature = Some("(x: i32) -> i32".to_string());
    node.docstring = Some("Does something useful".to_string());
    node.depth_from_main = Some(2);
    node.anchor_score = Some(0.3);
    overlay.insert_node(node);

    let occupancy = OccupancyMap::new();
    let result = explain_symbol(std::path::Path::new(""), &graph, &overlay, &occupancy, "documented_fn");
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("documented_fn"));
    assert!(text.contains("Function"));
    assert!(text.contains("signature") || text.contains("Documentation"));
}

#[test]
fn test_explain_symbol_not_found() {
    let (graph, overlay) = make_test_graph_with_nodes();

    // find_dead_code returns empty (not error), but explain_symbol should error
    let occupancy = OccupancyMap::new();
    let result = explain_symbol(std::path::Path::new(""), &graph, &overlay, &occupancy, "nonexistent_node_xyz");
    assert!(result.is_err());
}

#[test]
fn test_suggest_refactor_targets_empty() {
    let (graph, overlay) = make_test_graph_with_nodes();

    let result = suggest_refactor_targets(&graph, &overlay, 5);
    assert!(result.is_ok());
    let text = result.unwrap();
    // Should either show suggestions or say none found
    assert!(text.contains("Refactor") || text.contains("healthy") || text.contains("No nodes"));
}

#[test]
fn test_suggest_refactor_targets_with_debt() {
    let tmp = std::env::temp_dir().join("test_refactor_debt");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();
    let overlay = VolatileOverlay::new();

    // Create a high fan-in/fan-out node that might trigger debt scoring
    let mut node = GraphNode::new(NodeType::Class, "GodClass".to_string(), "/src/main.rs".to_string());
    node.fan_in = Some(15);
    node.fan_out = Some(15);
    node.anchor_score = Some(0.1);
    graph.upsert_node(node).unwrap();

    let result = suggest_refactor_targets(&graph, &overlay, 5);
    assert!(result.is_ok());
    // May or may not find targets depending on thresholds
    let text = result.unwrap();
    assert!(text.contains("Refactor") || text.contains("healthy") || text.contains("No nodes"));
}
// --- find_dead_code: unindexed files are not dead code (F-07) ---

/// A file whose call extraction failed produces functions with
/// `fan_in == 0 && fan_out == 0` — which used to be the *highest*
/// confidence tier for dead code. On the lain repo that made a
/// 1,127-line `watcher.rs` supply every one of the top 20 "highly
/// confident dead symbols"; all of them were live.
fn graph_with_an_unindexed_file() -> (GraphDatabase, VolatileOverlay) {
    let tmp = std::env::temp_dir().join("test_metrics_unindexed");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    // watcher.rs: four functions, no call edges recorded at all.
    for name in ["run_watcher_thread", "filter_event", "is_watched_file", "spawn_config_watcher"] {
        let mut n = GraphNode::new(NodeType::Function, name.to_string(), "/src/watcher.rs".to_string());
        n.fan_in = Some(0);
        n.fan_out = Some(0);
        graph.upsert_node(n).unwrap();
    }

    // lib.rs: indexed — its functions have call edges, and one genuine
    // orphan sits among them.
    let mut caller = GraphNode::new(NodeType::Function, "caller".to_string(), "/src/lib.rs".to_string());
    caller.fan_in = Some(1);
    caller.fan_out = Some(1);
    let mut callee = GraphNode::new(NodeType::Function, "callee".to_string(), "/src/lib.rs".to_string());
    callee.fan_in = Some(1);
    callee.fan_out = Some(0);
    let mut orphan = GraphNode::new(NodeType::Function, "orphan".to_string(), "/src/lib.rs".to_string());
    orphan.fan_in = Some(0);
    orphan.fan_out = Some(0);
    graph.upsert_node(caller).unwrap();
    graph.upsert_node(callee).unwrap();
    graph.upsert_node(orphan).unwrap();

    (graph, VolatileOverlay::new())
}

#[test]
fn unindexed_files_are_excluded_and_reported_not_called_dead() {
    let (graph, overlay) = graph_with_an_unindexed_file();
    let embedder = NlpEmbedder::new_stub();
    let cache = Arc::new(Mutex::new(HashMap::new()));

    let text = find_dead_code(std::path::Path::new(""), &graph, &overlay, None, &embedder, &cache).unwrap();

    // None of the watcher.rs symbols may be presented as dead.
    for name in ["run_watcher_thread", "filter_event", "is_watched_file", "spawn_config_watcher"] {
        assert!(
            !text.contains(&format!("- {name} (")),
            "unindexed symbol {name} must not be listed as dead:\n{text}"
        );
    }

    // The file is named as an indexing gap instead.
    assert!(
        text.contains("/src/watcher.rs"),
        "the excluded file must be reported so the extractor gap is actionable:\n{text}"
    );
    assert!(
        text.contains("indexing gap"),
        "the exclusion must be labelled an indexing gap:\n{text}"
    );

    // The genuine orphan in an indexed file still surfaces.
    assert!(
        text.contains("- orphan ("),
        "a real unreferenced symbol in an indexed file must still be found:\n{text}"
    );
}

#[test]
fn like_filter_errors_instead_of_silently_ignoring_the_query() {
    // With a stub embedder the semantic filter cannot run. It used to
    // fall through to the unfiltered list under a different label, so
    // every `like` query returned an identical result set.
    let (graph, overlay) = graph_with_an_unindexed_file();
    let embedder = NlpEmbedder::new_stub();
    let cache = Arc::new(Mutex::new(HashMap::new()));

    let result = find_dead_code(
        std::path::Path::new(""),
        &graph,
        &overlay,
        Some("presence claim"),
        &embedder,
        &cache,
    );
    let err = result.expect_err("`like` without a model must fail loudly");
    let msg = err.to_string();
    assert!(
        msg.contains("download-model") || msg.contains("LAIN_EMBEDDING_MODEL"),
        "the error must name a remedy that exists: {msg}"
    );
}

/// `explain_symbol` and `get_call_sites` must agree about callers.
///
/// `explain_symbol` rendered every incoming edge under "Called by",
/// including the `Defines` edge from the enclosing file — so a leaf
/// function reported `Called by: hooks.rs` (a file, not a caller) while
/// `get_call_sites` correctly called it a leaf. An agent cannot
/// arbitrate between two of lain's own tools.
#[test]
fn explain_symbol_does_not_report_a_defining_file_as_a_caller() {
    let tmp = std::env::temp_dir().join("test_metrics_call_graph_agreement");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();
    let overlay = VolatileOverlay::new();
    let occupancy = OccupancyMap::new();

    let file = GraphNode::new(NodeType::File, "hooks.rs".to_string(), "/src/cli/hooks.rs".to_string());
    let leaf = GraphNode::new(NodeType::Function, "sanitize".to_string(), "/src/cli/hooks.rs".to_string());
    graph.upsert_node(file.clone()).unwrap();
    graph.upsert_node(leaf.clone()).unwrap();

    // The only incoming edge is structural (File -> Symbol), not a call.
    graph.upsert_edge(GraphEdge::new(EdgeType::Contains, file.id.clone(), leaf.id.clone())).unwrap();

    let text = explain_symbol(
        std::path::Path::new(""),
        &graph,
        &overlay,
        &occupancy,
        "sanitize",
    )
    .unwrap();

    assert!(
        !text.contains("Called by"),
        "a Contains edge is not a call site:\n{text}"
    );
}

#[test]
fn explain_symbol_still_reports_real_callers() {
    let tmp = std::env::temp_dir().join("test_metrics_call_graph_real");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();
    let overlay = VolatileOverlay::new();
    let occupancy = OccupancyMap::new();

    let caller = GraphNode::new(NodeType::Function, "caller".to_string(), "/src/a.rs".to_string());
    let callee = GraphNode::new(NodeType::Function, "callee".to_string(), "/src/a.rs".to_string());
    graph.upsert_node(caller.clone()).unwrap();
    graph.upsert_node(callee.clone()).unwrap();
    graph.upsert_edge(GraphEdge::new(EdgeType::Calls, caller.id.clone(), callee.id.clone())).unwrap();

    let text = explain_symbol(std::path::Path::new(""), &graph, &overlay, &occupancy, "callee").unwrap();
    assert!(text.contains("Called by"), "a real Calls edge must show up:\n{text}");
    assert!(text.contains("caller"), "the caller must be named:\n{text}");
}

// --- Tests are not dead code ---

#[test]
fn test_symbols_are_excluded_from_dead_code() {
    let tmp = std::env::temp_dir().join("test_metrics_test_exclusion");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    // A test file's functions: no production caller, by design.
    for name in ["test_git_sensor_in_temp_repo", "test_repo_identity_invalid", "sample_event"] {
        let mut n = GraphNode::new(NodeType::Function, name.to_string(), "/src/server/git_tests.rs".to_string());
        n.fan_in = Some(0);
        n.fan_out = Some(1); // indexed: the file has call edges
        graph.upsert_node(n).unwrap();
    }
    // A genuinely unreferenced production function in an indexed file.
    let mut orphan = GraphNode::new(NodeType::Function, "orphan".to_string(), "/src/lib.rs".to_string());
    orphan.fan_in = Some(0);
    orphan.fan_out = Some(0);
    let mut live = GraphNode::new(NodeType::Function, "live".to_string(), "/src/lib.rs".to_string());
    live.fan_in = Some(2);
    live.fan_out = Some(3);
    let mut helper = GraphNode::new(NodeType::Function, "helper".to_string(), "/src/lib.rs".to_string());
    helper.fan_in = Some(1);
    helper.fan_out = Some(1);
    graph.upsert_node(orphan).unwrap();
    graph.upsert_node(live).unwrap();
    graph.upsert_node(helper).unwrap();

    let embedder = NlpEmbedder::new_stub();
    let cache = Arc::new(Mutex::new(HashMap::new()));
    let text = find_dead_code(std::path::Path::new(""), &graph, &VolatileOverlay::new(), None, &embedder, &cache).unwrap();

    for name in ["test_git_sensor_in_temp_repo", "test_repo_identity_invalid"] {
        assert!(
            !text.contains(&format!("- {name} (")),
            "a test function must not be reported as dead:\n{text}"
        );
    }
    assert!(
        text.contains("test symbol(s) were excluded"),
        "the exclusion must be stated, not silent:\n{text}"
    );
    assert!(
        text.contains("- orphan ("),
        "a real unreferenced production function must still be reported:\n{text}"
    );
}

#[test]
fn test_detection_covers_label_path_and_name() {
    use crate::server::tools::handlers::metrics::is_test_symbol_for_test;
    let mk = |name: &str, path: &str| GraphNode::new(NodeType::Function, name.to_string(), path.to_string());

    // Path conventions across languages.
    assert!(is_test_symbol_for_test(&mk("anything", "/src/server/git_tests.rs")));
    assert!(is_test_symbol_for_test(&mk("anything", "tests/presence.rs")));
    assert!(is_test_symbol_for_test(&mk("anything", "npm-shim/bin/lain.test.js")));
    // Name conventions.
    assert!(is_test_symbol_for_test(&mk("test_thing", "/src/lib.rs")));
    assert!(is_test_symbol_for_test(&mk("thing_test", "/src/lib.rs")));
    // The `#[test]` label, which the tree-sitter extractor sets.
    let mut labelled = mk("checks_a_thing", "/src/lib.rs");
    labelled.label = Some("test".to_string());
    assert!(is_test_symbol_for_test(&labelled));
    // Ordinary production code is untouched.
    assert!(!is_test_symbol_for_test(&mk("run_query", "/src/cli/query.rs")));
    assert!(!is_test_symbol_for_test(&mk("latest", "/src/server/graph.rs")));
}
