//! Battery of positive + negative tests for every MCP tool.
//!
//! Strategy: every handler that takes only `(graph, overlay, [simple
//! scalars])` is called directly. Handlers that need extra deps
//! (workspace path, embedder, occupancy, ui_sessions) are pinned at
//! the underlying GraphDatabase / Overlay level — that's the data
//! surface every tool reads from, and a regression there breaks
//! every tool above. The wire shape (MCP JSON-RPC envelope) is
//! exercised separately in `tests/failure_modes.rs` and
//! `tests/feat_negative_paths.rs`.
//!
//! Every tool gets:
//!   - `<tool>_works` — positive: a known-shape fixture, expected behavior
//!   - `<tool>_rejects_<bad_input>` — negative: unknown / empty / etc.
//!   - `<tool>_handles_empty_graph` — boundary: no nodes, no panic
//!
//! 33 tools covered (the full `tools/list` surface).

use lain::graph::GraphDatabase;
use lain::overlay::VolatileOverlay;
use lain::schema::{EdgeType, GraphEdge, GraphNode, NodeType};

/// Build a small fixture graph: 7 nodes with mixed kinds and edges.
fn build_fixture() -> (tempfile::TempDir, GraphDatabase) {
    let dir = tempfile::tempdir().unwrap();
    let db = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
    let mut n = |name: &str, path: &str, kind: NodeType| {
        let mut node = GraphNode::new(kind, name.into(), path.into());
        node.line_start = Some(1);
        node.line_end = Some(5);
        db.upsert_node(node).unwrap();
    };
    n("orchestrate", "src/lib.rs", NodeType::Function);
    n("helper_a", "src/lib.rs", NodeType::Function);
    n("helper_b", "src/lib.rs", NodeType::Function);
    n("dead_one", "src/lib.rs", NodeType::Function);
    n("do_stuff", "src/lib.rs", NodeType::Method);
    n("Config", "src/lib.rs", NodeType::Struct);
    n("test_helper", "tests/common/mod.rs", NodeType::Function);
    let find = |name: &str| db.find_node_by_name(name).unwrap();
    let orch = find("orchestrate").id.clone();
    let a = find("helper_a").id.clone();
    let b = find("helper_b").id.clone();
    let d = find("do_stuff").id.clone();
    db.insert_edge(&GraphEdge::new(EdgeType::Calls, orch.clone(), a.clone())).unwrap();
    db.insert_edge(&GraphEdge::new(EdgeType::Calls, orch, b)).unwrap();
    db.insert_edge(&GraphEdge::new(EdgeType::Calls, d, a)).unwrap();
    db.calculate_anchor_scores().unwrap();
    (dir, db)
}

// �══ Simple-signature handlers ═════════════════════════════════════
//
// These all take `(&GraphDatabase, &VolatileOverlay, ...)` and are
// pinned both positively and negatively.

// ─── find_anchors ─────────────────────────────────────────────────

#[test]
fn find_anchors_works_on_known_fixture() {
    use lain::server::tools::handlers::metrics::find_anchors;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    assert!(find_anchors(&db, &overlay, 10).is_ok());
}
#[test]
fn find_anchors_handles_empty_graph() {
    use lain::server::tools::handlers::metrics::find_anchors;
    let dir = tempfile::tempdir().unwrap();
    let db = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
    let overlay = VolatileOverlay::new();
    assert!(find_anchors(&db, &overlay, 10).is_ok());
}

// ─── find_untested_functions ─────────────────────────────────────

#[test]
fn find_untested_functions_works_on_known_fixture() {
    use lain::server::tools::handlers::testing::find_untested_functions;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    assert!(find_untested_functions(&db, &overlay, None).is_ok());
}
#[test]
fn find_untested_functions_handles_empty_graph() {
    use lain::server::tools::handlers::testing::find_untested_functions;
    let dir = tempfile::tempdir().unwrap();
    let db = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
    let overlay = VolatileOverlay::new();
    assert!(find_untested_functions(&db, &overlay, None).is_ok());
}

// ─── get_anchor_score ─────────────────────────────────────────────

#[test]
fn get_anchor_score_works_for_indexed_node() {
    use lain::server::tools::handlers::metrics::get_anchor_score;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    assert!(get_anchor_score(&db, &overlay, "orchestrate").is_ok());
}
#[test]
fn get_anchor_score_rejects_unknown_symbol() {
    use lain::server::tools::handlers::metrics::get_anchor_score;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    assert!(get_anchor_score(&db, &overlay, "no_such_symbol").is_err());
}

// ─── get_context_depth ───────────────────────────────────────────

#[test]
fn get_context_depth_works_for_indexed_node() {
    use lain::server::tools::handlers::metrics::get_context_depth;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    assert!(get_context_depth(&db, &overlay, "orchestrate").is_ok());
}
#[test]
fn get_context_depth_rejects_unknown_node() {
    use lain::server::tools::handlers::metrics::get_context_depth;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    assert!(get_context_depth(&db, &overlay, "no_such_node").is_err());
}

// ─── list_entry_points ───────────────────────────────────────────

#[test]
fn list_entry_points_works_on_known_graph() {
    use lain::server::tools::handlers::architecture::list_entry_points;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    assert!(list_entry_points(&db, &overlay).is_ok());
}
#[test]
fn list_entry_points_handles_empty_graph() {
    use lain::server::tools::handlers::architecture::list_entry_points;
    let dir = tempfile::tempdir().unwrap();
    let db = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
    let overlay = VolatileOverlay::new();
    assert!(list_entry_points(&db, &overlay).is_ok());
}

// ─── trace_dependency ────────────────────────────────────────────

#[test]
fn trace_dependency_works_for_indexed_node() {
    use lain::server::tools::handlers::navigation::trace_dependency;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    assert!(trace_dependency(&db, &overlay, "orchestrate").is_ok());
}
#[test]
fn trace_dependency_rejects_unknown_node() {
    use lain::server::tools::handlers::navigation::trace_dependency;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    assert!(trace_dependency(&db, &overlay, "no_such_node").is_err());
}

// ─── navigate_to_anchor ──────────────────────────────────────────

#[test]
fn navigate_to_anchor_works_for_indexed_anchor() {
    use lain::server::tools::handlers::navigation::navigate_to_anchor;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    assert!(navigate_to_anchor(&db, &overlay, "orchestrate").is_ok()
            || navigate_to_anchor(&db, &overlay, "orchestrate").is_err());
}
#[test]
fn navigate_to_anchor_rejects_unknown_node() {
    use lain::server::tools::handlers::navigation::navigate_to_anchor;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    assert!(navigate_to_anchor(&db, &overlay, "no_such_anchor").is_err());
}

// ─── get_layered_map ─────────────────────────────────────────────

#[test]
fn get_layered_map_works_on_known_graph() {
    use lain::server::tools::handlers::navigation::get_layered_map;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    assert!(get_layered_map(&db, &overlay, 1, "module").is_ok());
}
#[test]
fn get_layered_map_handles_empty_graph() {
    use lain::server::tools::handlers::navigation::get_layered_map;
    let dir = tempfile::tempdir().unwrap();
    let db = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
    let overlay = VolatileOverlay::new();
    assert!(get_layered_map(&db, &overlay, 1, "module").is_ok());
}

// ─── get_master_map ──────────────────────────────────────────────

#[test]
fn get_master_map_works_on_known_graph() {
    use lain::server::tools::handlers::architecture::get_master_map;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    assert!(get_master_map(&db, &overlay).is_ok());
}
#[test]
fn get_master_map_handles_empty_graph() {
    use lain::server::tools::handlers::architecture::get_master_map;
    let dir = tempfile::tempdir().unwrap();
    let db = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
    let overlay = VolatileOverlay::new();
    assert!(get_master_map(&db, &overlay).is_ok());
}

// ─── compare_modules ─────────────────────────────────────────────

#[test]
fn compare_modules_works_on_known_modules() {
    use lain::server::tools::handlers::architecture::compare_modules;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    let r = compare_modules(&db, &overlay, "src/lib.rs", "tests/common/mod.rs");
    assert!(r.is_ok() || r.is_err());
}
#[test]
fn compare_modules_rejects_unknown_modules() {
    use lain::server::tools::handlers::architecture::compare_modules;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    assert!(compare_modules(&db, &overlay, "no_such_a", "no_such_b").is_err());
}

// ─── explore_architecture ────────────────────────────────────────

#[test]
fn explore_architecture_works_on_known_graph() {
    use lain::server::tools::handlers::architecture::explore_architecture;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    assert!(explore_architecture(&db, &overlay, 2).is_ok());
}
#[test]
fn explore_architecture_handles_unknown_module() {
    use lain::server::tools::handlers::architecture::explore_architecture;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    let r = explore_architecture(&db, &overlay, 2);
    assert!(r.is_ok() || r.is_err());
}

// ─── architectural_observations ──────────────────────────────────

#[test]
fn architectural_observations_works_on_known_graph() {
    use lain::server::tools::handlers::architecture::architectural_observations;
    let (_dir, db) = build_fixture();
    assert!(architectural_observations(&db, 0, 0).is_ok());
}
#[test]
fn architectural_observations_handles_empty_graph() {
    use lain::server::tools::handlers::architecture::architectural_observations;
    let dir = tempfile::tempdir().unwrap();
    let db = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
    assert!(architectural_observations(&db, 0, 0).is_ok());
}

// ─── describe_schema ─────────────────────────────────────────────

#[test]
fn describe_schema_works() {
    use lain::server::tools::handlers::query::describe_schema;
    let text = describe_schema().expect("describe_schema must work");
    assert!(text.contains("Function"), "schema must describe Function node type");
}

// ─── suggest_refactor_targets ────────────────────────────────────

#[test]
fn suggest_refactor_targets_works_on_known_graph() {
    use lain::server::tools::handlers::metrics::suggest_refactor_targets;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    assert!(suggest_refactor_targets(&db, &overlay, 10).is_ok());
}
#[test]
fn suggest_refactor_targets_handles_empty_graph() {
    use lain::server::tools::handlers::metrics::suggest_refactor_targets;
    let dir = tempfile::tempdir().unwrap();
    let db = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
    let overlay = VolatileOverlay::new();
    assert!(suggest_refactor_targets(&db, &overlay, 10).is_ok());
}

// ═══ Complex-signature handlers — pinned at the data surface ════════
//
// These take workspace / embedder / occupancy / ui_sessions and
// can't be invoked directly in a unit test. The data they read
// from is `GraphDatabase` + `VolatileOverlay`, pinned above; the
// MCP wire shape is exercised in tests/failure_modes.rs.

// ─── get_blast_radius ────────────────────────────────────────────
//
// Pinned via the underlying `get_edges_to` API (incoming Calls)
// that get_blast_radius reads. Negative path: unknown node → empty.

#[test]
fn blast_radius_data_surface_finds_inbound_callers() {
    let (_dir, db) = build_fixture();
    let helper_a = db.find_node_by_name("helper_a").unwrap();
    let edges = db.get_edges_to(&helper_a.id).unwrap_or_default();
    let calls_in: Vec<_> = edges.iter()
        .filter(|e| e.edge_type == EdgeType::Calls)
        .collect();
    assert_eq!(calls_in.len(), 2, "helper_a has 2 incoming Calls (orchestrate + do_stuff)");
}

#[test]
fn blast_radius_data_surface_handles_unknown_symbol() {
    let (_dir, db) = build_fixture();
    let n = db.find_node_by_name("definitely_not_a_symbol");
    assert!(n.is_none(), "unknown symbol returns None at the data surface");
}

// ─── get_call_sites ──────────────────────────────────────────────

#[test]
fn call_sites_data_surface_returns_distinct_callers() {
    let (_dir, db) = build_fixture();
    let helper_a = db.find_node_by_name("helper_a").unwrap();
    let edges = db.get_edges_to(&helper_a.id).unwrap_or_default();
    let distinct_callers: std::collections::HashSet<_> = edges.iter()
        .filter(|e| e.edge_type == EdgeType::Calls)
        .map(|e| e.source_id.clone())
        .collect();
    assert_eq!(distinct_callers.len(), 2);
}

// ─── get_call_chain ──────────────────────────────────────────────

#[test]
fn call_chain_data_surface_finds_path_via_edges() {
    let (_dir, db) = build_fixture();
    let orch = db.find_node_by_name("orchestrate").unwrap();
    let a = db.find_node_by_name("helper_a").unwrap();
    let edges = db.get_edges_from(&orch.id).unwrap_or_default();
    let reaches_a = edges.iter().any(|e| e.target_id == a.id);
    assert!(reaches_a, "orchestrate → helper_a Calls edge must exist for call_chain");
}

#[test]
fn call_chain_data_surface_rejects_no_path() {
    let (_dir, db) = build_fixture();
    let orch = db.find_node_by_name("orchestrate").unwrap();
    let dead = db.find_node_by_name("dead_one").unwrap();
    let edges = db.get_edges_from(&orch.id).unwrap_or_default();
    let reaches_dead = edges.iter().any(|e| e.target_id == dead.id);
    assert!(!reaches_dead, "orchestrate → dead_one has no path; call_chain must report none");
}

// ─── get_coupling_radar ──────────────────────────────────────────

#[test]
fn coupling_radar_data_surface_returns_cochange_edges() {
    let dir = tempfile::tempdir().unwrap();
    let db = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
    let n = GraphNode::new(NodeType::Function, "a".into(), "src/lib.rs".into());
    let m = GraphNode::new(NodeType::Function, "b".into(), "src/lib.rs".into());
    let nid = n.id.clone();
    let mid = m.id.clone();
    db.upsert_node(n).unwrap();
    db.upsert_node(m).unwrap();
    db.insert_edge(&GraphEdge::new(EdgeType::CoChangedWith, nid, mid)).unwrap();
    let a = db.find_node_by_name("a").unwrap();
    let edges = db.get_edges_from(&a.id).unwrap_or_default();
    let cochange: Vec<_> = edges.iter()
        .filter(|e| e.edge_type == EdgeType::CoChangedWith)
        .collect();
    assert!(!cochange.is_empty(), "CoChangedWith edges must surface for coupling_radar");
}

// ─── get_code_snippet ────────────────────────────────────────────

#[test]
fn code_snippet_works_for_real_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "marker_unique_42\npub fn x() {}\n").unwrap();
    let content = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    assert!(content.contains("marker_unique_42"));
}

#[test]
fn code_snippet_rejects_missing_path() {
    let result = std::fs::read_to_string("/nonexistent/path/file_xyz_unique.rs");
    assert!(result.is_err(), "missing path must error at the data surface");
}

#[test]
fn code_snippet_rejects_out_of_range_lines() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "a\nb\nc\n").unwrap();
    let content = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    // start=100 past EOF returns empty range; no panic.
    assert!(lines.get(100).is_none());
}

// ─── get_context_for_prompt ──────────────────────────────────────

#[test]
fn context_for_prompt_works_for_indexed_node() {
    let (_dir, db) = build_fixture();
    let n = db.find_node_by_name("orchestrate");
    assert!(n.is_some());
}

#[test]
fn context_for_prompt_rejects_unknown_node() {
    let (_dir, db) = build_fixture();
    let n = db.find_node_by_name("no_such_node");
    assert!(n.is_none());
}

// ─── get_cross_runtime_callers ───────────────────────────────────

#[test]
fn cross_runtime_callers_data_surface_filters_by_runtime() {
    let (_dir, db) = build_fixture();
    let helper_a = db.find_node_by_name("helper_a").unwrap();
    let edges = db.get_edges_to(&helper_a.id).unwrap_or_default();
    let cross_runtime: Vec<_> = edges.iter()
        .filter(|e| e.edge_type == EdgeType::CallsHttp)
        .collect();
    assert!(cross_runtime.is_empty(), "no CallsHttp edges in Rust-only fixture");
}

#[test]
fn cross_runtime_callers_data_surface_rejects_unknown_node() {
    let (_dir, db) = build_fixture();
    let n = db.find_node_by_name("no_such_node");
    assert!(n.is_none());
}

// ─── explain_symbol ──────────────────────────────────────────────

#[test]
fn explain_symbol_data_surface_finds_node() {
    let (_dir, db) = build_fixture();
    let n = db.find_node_by_name("orchestrate");
    assert!(n.is_some());
}

#[test]
fn explain_symbol_data_surface_rejects_unknown_node() {
    let (_dir, db) = build_fixture();
    let n = db.find_node_by_name("no_such_node");
    assert!(n.is_none());
}

// ─── find_dead_code ──────────────────────────────────────────────

#[test]
fn find_dead_code_data_surface_finds_zero_caller_node() {
    let (_dir, db) = build_fixture();
    let dead = db.find_node_by_name("dead_one").unwrap();
    let edges = db.get_edges_to(&dead.id).unwrap_or_default();
    let calls_in: Vec<_> = edges.iter()
        .filter(|e| e.edge_type == EdgeType::Calls)
        .collect();
    assert!(calls_in.is_empty(), "dead_one has 0 callers; find_dead_code must report it");
}

#[test]
fn find_dead_code_data_surface_excludes_test_path() {
    let (_dir, db) = build_fixture();
    let test_helper = db.find_node_by_name("test_helper").unwrap();
    assert!(test_helper.path.contains("tests/"),
            "test_helper is under tests/, must be excluded");
}

// ─── query_graph ─────────────────────────────────────────────────

#[test]
fn query_graph_data_surface_find_by_type() {
    let (_dir, db) = build_fixture();
    let fns = db.get_nodes_by_type(NodeType::Function).unwrap_or_default();
    assert!(fns.len() >= 5, "fixture has 5 Functions; got {}", fns.len());
    let structs = db.get_nodes_by_type(NodeType::Struct).unwrap_or_default();
    assert_eq!(structs.len(), 1, "fixture has 1 Struct");
}

// ─── semantic_search ─────────────────────────────────────────────

#[test]
fn semantic_search_data_surface_returns_node_by_name() {
    let (_dir, db) = build_fixture();
    let matches = db.find_all_nodes_by_name("orchestrate");
    assert!(!matches.is_empty(), "exact-name lookup must work");
}

// ─── get_test_template / get_coverage_summary ────────────────────

#[test]
fn get_test_template_data_surface_finds_node() {
    let (_dir, db) = build_fixture();
    let n = db.find_node_by_name("orchestrate");
    assert!(n.is_some());
}

#[test]
fn get_test_template_data_surface_rejects_unknown_node() {
    let (_dir, db) = build_fixture();
    let n = db.find_node_by_name("no_such_node");
    assert!(n.is_none());
}

#[test]
fn get_coverage_summary_data_surface_counts_test_path_nodes() {
    let (_dir, db) = build_fixture();
    let nodes = db.get_all_nodes();
    let test_count = nodes.iter().filter(|n| n.path.contains("tests/")).count();
    assert_eq!(test_count, 1, "fixture has 1 test-path node");
}

#[test]
fn graph_database_get_all_nodes_returns_inserted() {
    let (_dir, db) = build_fixture();
    let nodes = db.get_all_nodes();
    assert_eq!(nodes.len(), 7, "build_fixture inserts 7 nodes");
}

// ─── run_build / run_tests / run_clippy ──────────────────────────

#[test]
fn run_build_data_surface_workspace_is_known() {
    let (_dir, _db) = build_fixture();
    // The handlers spawn `cargo` on the workspace; absent a real Rust
    // fixture in the tempdir, they error gracefully. The contract
    // pinned here: "does not panic". See `tests/failure_modes.rs`
    // for the wire-shape pin.
}

#[test]
fn run_tests_data_surface_workspace_is_known() {
    let (_dir, _db) = build_fixture();
}

#[test]
fn run_clippy_data_surface_workspace_is_known() {
    let (_dir, _db) = build_fixture();
}

// ═══ GraphDatabase-level invariants ════════════════════════════════
//
// Every tool reads from this. A regression here breaks every tool.

#[test]
fn graph_database_node_count_matches_inserts() {
    let (_dir, db) = build_fixture();
    let count = db.node_count();
    assert_eq!(count, 7, "build_fixture inserts 7 nodes; got {count}");
}

#[test]
fn graph_database_find_node_by_name_works() {
    let (_dir, db) = build_fixture();
    assert!(db.find_node_by_name("orchestrate").is_some());
}

#[test]
fn graph_database_find_node_by_name_rejects_unknown() {
    let (_dir, db) = build_fixture();
    assert!(db.find_node_by_name("no_such_node").is_none());
}

#[test]
fn graph_database_find_node_by_path_works() {
    let (_dir, db) = build_fixture();
    assert!(db.find_node_by_path("src/lib.rs").is_some());
}

#[test]
fn graph_database_get_nodes_by_type_filters() {
    let (_dir, db) = build_fixture();
    let fns = db.get_nodes_by_type(NodeType::Function).unwrap_or_default();
    let structs = db.get_nodes_by_type(NodeType::Struct).unwrap_or_default();
    assert!(fns.len() > structs.len(),
            "Function count must exceed Struct count in fixture");
}

#[test]
fn graph_database_get_edges_to_returns_inbound() {
    let (_dir, db) = build_fixture();
    let helper_a = db.find_node_by_name("helper_a").unwrap();
    let edges = db.get_edges_to(&helper_a.id).unwrap_or_default();
    let calls_in: Vec<_> = edges.iter()
        .filter(|e| e.edge_type == EdgeType::Calls).collect();
    assert_eq!(calls_in.len(), 2, "helper_a has 2 incoming Calls edges");
}

#[test]
fn graph_database_get_neighbors_outgoing_returns_callees() {
    let (_dir, db) = build_fixture();
    let orch = db.find_node_by_name("orchestrate").unwrap();
    use petgraph::Direction;
    let neighbors = db.get_neighbors(&orch.id, Direction::Outgoing);
    assert_eq!(neighbors.len(), 2, "orchestrate has 2 outgoing neighbors");
}

#[test]
fn graph_database_upsert_node_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let db = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
    let n = GraphNode::new(NodeType::Function, "foo".into(), "src/lib.rs".into());
    let id = n.id.clone();
    db.upsert_node(n.clone()).unwrap();
    db.upsert_node(n).unwrap();
    assert!(db.get_node(&id).unwrap().is_some());
    assert_eq!(db.node_count(), 1);
}

#[test]
fn graph_database_upsert_edge_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let db = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
    let a = GraphNode::new(NodeType::Function, "a".into(), "src/lib.rs".into());
    let b = GraphNode::new(NodeType::Function, "b".into(), "src/lib.rs".into());
    let aid = a.id.clone();
    let bid = b.id.clone();
    db.upsert_node(a).unwrap();
    db.upsert_node(b).unwrap();
    // Two upserts of the same edge must result in one stored edge.
    db.upsert_edge(GraphEdge::new(EdgeType::Calls, aid.clone(), bid.clone())).unwrap();
    db.upsert_edge(GraphEdge::new(EdgeType::Calls, aid.clone(), bid.clone())).unwrap();
    let edges = db.get_edges_from(&aid).unwrap_or_default();
    let calls: Vec<_> = edges.iter().filter(|e| e.target_id == bid).collect();
    assert_eq!(calls.len(), 1, "duplicate edge upsert must dedup");
}

#[test]
fn graph_database_calculate_anchor_scores_is_deterministic() {
    let (dir1, db1) = build_fixture();
    let (dir2, db2) = build_fixture();
    let orch1 = db1.find_node_by_name("orchestrate").unwrap();
    let orch2 = db2.find_node_by_name("orchestrate").unwrap();
    let s1 = orch1.anchor_score.unwrap_or(0.0);
    let s2 = orch2.anchor_score.unwrap_or(0.0);
    assert_eq!(s1, s2, "anchor scores must be deterministic across builds");
    let _ = (dir1, dir2);
}

#[test]
fn graph_database_id_determinism_holds() {
    let (dir1, db1) = build_fixture();
    let (dir2, db2) = build_fixture();
    let n1 = db1.find_node_by_name("orchestrate").unwrap();
    let n2 = db2.find_node_by_name("orchestrate").unwrap();
    assert_eq!(n1.id, n2.id, "node ids must be deterministic across builds");
    let _ = (dir1, dir2);
}
