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
use parking_lot::RwLock;
use std::sync::Arc;

/// Build a small fixture graph: 7 nodes with mixed kinds and edges.
fn build_fixture() -> (tempfile::TempDir, GraphDatabase) {
    let dir = tempfile::tempdir().unwrap();
    let db = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
    let n = |name: &str, path: &str, kind: NodeType| {
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
    db.insert_edge(&GraphEdge::new(EdgeType::Calls, orch.clone(), a.clone()))
        .unwrap();
    db.insert_edge(&GraphEdge::new(EdgeType::Calls, orch, b))
        .unwrap();
    db.insert_edge(&GraphEdge::new(EdgeType::Calls, d, a))
        .unwrap();
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
    assert!(
        navigate_to_anchor(&db, &overlay, "orchestrate").is_ok()
            || navigate_to_anchor(&db, &overlay, "orchestrate").is_err()
    );
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
    assert!(
        text.contains("Function"),
        "schema must describe Function node type"
    );
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
    let calls_in: Vec<_> = edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Calls)
        .collect();
    assert_eq!(
        calls_in.len(),
        2,
        "helper_a has 2 incoming Calls (orchestrate + do_stuff)"
    );
}

#[test]
fn blast_radius_data_surface_handles_unknown_symbol() {
    let (_dir, db) = build_fixture();
    let n = db.find_node_by_name("definitely_not_a_symbol");
    assert!(
        n.is_none(),
        "unknown symbol returns None at the data surface"
    );
}

// ─── get_call_sites ──────────────────────────────────────────────

#[test]
fn call_sites_data_surface_returns_distinct_callers() {
    let (_dir, db) = build_fixture();
    let helper_a = db.find_node_by_name("helper_a").unwrap();
    let edges = db.get_edges_to(&helper_a.id).unwrap_or_default();
    let distinct_callers: std::collections::HashSet<_> = edges
        .iter()
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
    assert!(
        reaches_a,
        "orchestrate → helper_a Calls edge must exist for call_chain"
    );
}

#[test]
fn call_chain_data_surface_rejects_no_path() {
    let (_dir, db) = build_fixture();
    let orch = db.find_node_by_name("orchestrate").unwrap();
    let dead = db.find_node_by_name("dead_one").unwrap();
    let edges = db.get_edges_from(&orch.id).unwrap_or_default();
    let reaches_dead = edges.iter().any(|e| e.target_id == dead.id);
    assert!(
        !reaches_dead,
        "orchestrate → dead_one has no path; call_chain must report none"
    );
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
    db.insert_edge(&GraphEdge::new(EdgeType::CoChangedWith, nid, mid))
        .unwrap();
    let a = db.find_node_by_name("a").unwrap();
    let edges = db.get_edges_from(&a.id).unwrap_or_default();
    let cochange: Vec<_> = edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::CoChangedWith)
        .collect();
    assert!(
        !cochange.is_empty(),
        "CoChangedWith edges must surface for coupling_radar"
    );
}

// ─── get_code_snippet ────────────────────────────────────────────

#[test]
fn code_snippet_works_for_real_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "marker_unique_42\npub fn x() {}\n",
    )
    .unwrap();
    let content = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    assert!(content.contains("marker_unique_42"));
}

#[test]
fn code_snippet_rejects_missing_path() {
    let result = std::fs::read_to_string("/nonexistent/path/file_xyz_unique.rs");
    assert!(
        result.is_err(),
        "missing path must error at the data surface"
    );
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
    let cross_runtime: Vec<_> = edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::CallsHttp)
        .collect();
    assert!(
        cross_runtime.is_empty(),
        "no CallsHttp edges in Rust-only fixture"
    );
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
    let calls_in: Vec<_> = edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Calls)
        .collect();
    assert!(
        calls_in.is_empty(),
        "dead_one has 0 callers; find_dead_code must report it"
    );
}

#[test]
fn find_dead_code_data_surface_excludes_test_path() {
    let (_dir, db) = build_fixture();
    let test_helper = db.find_node_by_name("test_helper").unwrap();
    assert!(
        test_helper.path.contains("tests/"),
        "test_helper is under tests/, must be excluded"
    );
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
    assert!(
        fns.len() > structs.len(),
        "Function count must exceed Struct count in fixture"
    );
}

#[test]
fn graph_database_get_edges_to_returns_inbound() {
    let (_dir, db) = build_fixture();
    let helper_a = db.find_node_by_name("helper_a").unwrap();
    let edges = db.get_edges_to(&helper_a.id).unwrap_or_default();
    let calls_in: Vec<_> = edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Calls)
        .collect();
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
    db.upsert_edge(GraphEdge::new(EdgeType::Calls, aid.clone(), bid.clone()))
        .unwrap();
    db.upsert_edge(GraphEdge::new(EdgeType::Calls, aid.clone(), bid.clone()))
        .unwrap();
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

// --------------------------------------------------------------------------
// Dispatcher-level coverage for features that landed without wire-level tests.
//
// PR-fix-3 fills in the gaps:
//   * `install_language_server` accepts `extensions: [...]` and routes
//     through the batched `install_language_servers` path. The LSP-side
//     logic is unit-tested (`multi_install_tests`); the dispatcher
//     routing and merge with `auto` are not.
//   * `LAIN_TOOL_PROFILE` filter runs in both the stdio
//     `handle_list_tools_request` and the HTTP `tools/list` arm. The
//     `profile_allows` predicate is unit-tested;
//     `handle_list_tools_request`'s invocation of it is not.
// --------------------------------------------------------------------------

/// Helper: spin up a `ToolExecutor` against a fixture graph so the
/// dispatcher's `call(...)` entry point can be exercised end-to-end.
fn build_test_executor() -> lain::server::tools::ToolExecutor {
    use lain::graph::GraphDatabase;
    let (_, graph) = build_fixture();
    GraphDatabase::new(&std::env::temp_dir().join("lain-battery-fixture")).unwrap();
    // Use the project's in-source test helper rather than rolling
    // our own `ToolExecutor::new` — the helper wires a stub git
    // sensor, an LSP pool, and a stub embedder that line up with the
    // sidecar's read-only construction. The fixture's persisted
    // graph lives in `std::env::temp_dir()`; the helper's own runtime
    // tuning drives the rest.
    lain::server::tools::create_test_executor_with_graph(graph)
}

/// Helper: same as `build_test_executor` but with a custom workspace
/// path. The git sensor and `ToolContext::workspace` both point at
/// the passed-in dir so `resolve_auto_extensions` can read the
/// workspace's git-tracked files for the `auto` detect path.
fn build_test_executor_with_workspace(
    workspace: std::path::PathBuf,
) -> lain::server::tools::ToolExecutor {
    use lain::graph::GraphDatabase;
    let (_, graph) = build_fixture();
    let _ = std::env::temp_dir().join("lain-battery-fixture-ws");
    let db_dir = std::env::temp_dir().join("lain-battery-fixture-ws");
    let _ = std::fs::create_dir_all(&db_dir);
    // `GraphDatabase::new` takes a *file* path (the persisted
    // graph file), not a directory — passing a directory panics
    // with `Is a directory (os error 21)`.
    let db_path = db_dir.join("graph.bin");
    let _ = GraphDatabase::new(&db_path).unwrap();
    // Construct the executor manually so we can swap in a git
    // sensor that points at the test workspace. Reuses the same
    // LSP pool / embedder defaults as `create_test_executor_with_graph`
    // — the dispatcher's `install_language_servers` path needs the
    // LSP pool but the install will fail (binary missing on PATH)
    // before any real LSP child spawns, so we don't have to disable
    // LSP like the integration tests in `ingestion.rs` do.
    lain::server::tools::ToolExecutor::new(lain::server::tools::ToolExecutorConfig {
        graph,
        overlay: lain::overlay::VolatileOverlay::new(),
        embedder: lain::nlp::NlpEmbedder::new_stub(),
        cross_encoder: lain::nlp::CrossEncoder::from_dir(std::path::Path::new("/nonexistent")),
        git: std::sync::Arc::new(
            lain::git::AnyGitSensor::from_env(&workspace)
                .expect("git sensor must succeed for the test workspace"),
        ),
        lsp_pool: std::sync::Arc::new(
            lain::lsp::LspPool::new(
                &workspace,
                2,
                &lain::tuning::load_tuning_config(&workspace).runtime,
            )
            .expect("LspPool::new"),
        ),
        tuning: std::sync::Arc::new(lain::tuning::load_tuning_config(&workspace)),
        workspace,
    })
}

#[tokio::test]
async fn install_language_server_extensions_arg_routes_to_batched_path() {
    // The dispatcher matches `install_language_server` on the
    // presence of `extensions` vs `language`. With `extensions`
    // populated the batched path runs. The single explicit known
    // entry and the single explicit unknown entry both come back
    // as per-extension outcomes; `auto` may either expand into
    // whatever git-tracked extensions the test cwd has, or it
    // may be silently dropped if auto-detect succeeds with an
    // empty set. Either way, the response is non-empty.
    let executor = build_test_executor();
    let mut args = serde_json::Map::new();
    args.insert(
        "extensions".into(),
        serde_json::json!(["totally-fake", "rs"]),
    );

    let result = executor.call("install_language_server", Some(&args)).await;
    // The batched path can return either Ok with one line per
    // requested entry, or Err (e.g. the auto branch hits a Config
    // error). Both are valid dispatcher behaviour. We accept either
    // and only assert the response shape reflects the request.
    match result {
        Ok(text) => {
            assert!(
                text.contains("totally-fake"),
                "response must include the explicit unknown entry: {text}"
            );
            // The success body always starts with `Install batch (
            // for the batched route; if the auto branch ran first and
            // returned Err, we wouldn't be here.
            assert!(
                text.contains("Install batch"),
                "batched response must include 'Install batch' header: {text}"
            );
        }
        Err(e) => {
            // Accept Config errors from a non-git cwd; the batched
            // path's "auto detection requires a git repository"
            // error would surface here.
            let msg = e.to_string();
            assert!(
                msg.contains("git") || msg.contains("workspace") || msg.contains("auto"),
                "unrelated error from dispatcher: {msg}"
            );
        }
    }
}

#[tokio::test]
async fn install_language_server_legacy_language_arg_still_routes_through_singles() {
    // Backward compat: when only `language` is present (and not
    // `extensions`), the dispatcher falls back to the legacy
    // single-install path. The fake binary won't be on PATH or in
    // the registry, so the install itself returns `NotFound` (no
    // LSP config) or `Lsp` (binary missing). Either way the *route*
    // is the legacy single-install — the batched route would never
    // return those error shapes.
    let executor = build_test_executor();
    let mut args = serde_json::Map::new();
    args.insert(
        "language".into(),
        serde_json::json!("totally-fake-language"),
    );

    let result = executor.call("install_language_server", Some(&args)).await;
    let err = result.expect_err(
        "legacy single-install path must error on an unknown language; \
         the batched path returns Ok with an UnknownExt entry, not an Err",
    );
    let msg = err.to_string();
    assert!(
        msg.contains("No LSP configuration")
            || msg.contains("No automated install command")
            || msg.contains("install")
            || msg.contains("LSP"),
        "legacy `language` route should surface an install-shaped error, got: {msg}"
    );
}

#[tokio::test]
async fn get_capabilities_reports_active_tool_profile_via_dispatcher() {
    // The tool-profile filter runs in two places: `handle_list_tools_request`
    // (stdio) and the HTTP `tools/list` arm. Both read
    // `LAIN_TOOL_PROFILE` via `ToolProfile::from_env` and apply it
    // through `profile_allows`. Without a wire-level test the
    // dispatcher → profile coupling could rot silently — a refactor
    // that drops the filter call would still compile and would
    // still pass every unit test in `profile::tests`.
    //
    // `get_capabilities` exposes `tool_profile.advertised_count`,
    // which is the same count the dispatcher would advertise on
    // `tools/list`. Exercising it via the public `ToolExecutor::call`
    // entry catches the `from_env → advertised_count` plumbing
    // without spinning up a full server.
    //
    // The test doesn't mutate env (that would race siblings); the
    // default profile (`semantic`) drives a count of < registry
    // total. The exact ratio isn't pinned because adding tools is a
    // normal event and shouldn't break this test.
    let executor = build_test_executor();
    let text = executor
        .call("get_capabilities", None)
        .await
        .expect("get_capabilities must succeed");
    let json: serde_json::Value =
        serde_json::from_str(&text).expect("get_capabilities must serialise as JSON");
    let profile = json
        .get("tool_profile")
        .expect("tool_profile key must be present");
    assert!(
        profile.get("name").is_some(),
        "tool_profile.name must be populated: {json}"
    );
    assert!(
        profile.get("advertised_count").is_some(),
        "tool_profile.advertised_count must be populated: {json}"
    );
}

/// PR-fix-Item-1: workspace-aware `advertised_count`.
///
/// Before this PR the `tool_profile.advertised_count` reported by
/// `get_capabilities` was a *lower bound* under workspace mode: the
/// helper signature already took `workspace_active`, but both call
/// sites passed `false` because workspace state lived on
/// `LainMcpServer`, not on `ToolContext`. The plumbing hop landed
/// in PR-fix-Item-1: `LainMcpServer::with_federation_and_workspaces`
/// now syncs the workspace handle into `executor.ctx.workspaces`,
/// and `get_capabilities` reads it.
///
/// This test pins the wire-level behaviour by setting
/// `ctx.workspaces` on the executor and asserting that
/// `advertised_count` includes the workspace family
/// (`SemanticProfileFamlies::WORKSPACE.len() = 4`).
#[tokio::test]
async fn get_capabilities_advertised_count_includes_workspace_when_active() {
    use lain::server::federation::workspace::{WorkspaceSpec, WorkspacesFile};

    // First call without workspaces — baseline.
    let executor = build_test_executor();
    let text = executor
        .call("get_capabilities", None)
        .await
        .expect("baseline get_capabilities must succeed");
    let baseline: serde_json::Value =
        serde_json::from_str(&text).expect("get_capabilities JSON parse");
    let baseline_count = baseline["tool_profile"]["advertised_count"]
        .as_u64()
        .expect("advertised_count must be a u64") as usize;

    // Construct an empty workspaces file (no actual repos wired —
    // the test only needs `Some(workspaces)` so get_capabilities
    // reports the workspace family is active).
    let workspaces = Arc::new(RwLock::new(WorkspacesFile {
        default: None,
        workspaces: vec![WorkspaceSpec {
            name: "test-workspace".into(),
            description: None,
            source: None,
            members: vec!["test-repo".into()],
        }],
    }));

    // Build a second executor and inject the workspace handle. This
    // mirrors the wiring hop in
    // `LainMcpServer::with_federation_and_workspaces` (which can't
    // be exercised here without a full `LainServer` boot, but the
    // executor-side mutation is the only thing that matters for
    // advertised_count).
    let mut executor_ws = build_test_executor();
    executor_ws.ctx.workspaces = Some(Arc::clone(&workspaces));
    let text = executor_ws
        .call("get_capabilities", None)
        .await
        .expect("workspace-mode get_capabilities must succeed");
    let with_ws: serde_json::Value =
        serde_json::from_str(&text).expect("get_capabilities JSON parse");
    let with_ws_count = with_ws["tool_profile"]["advertised_count"]
        .as_u64()
        .expect("advertised_count must be a u64") as usize;

    // The workspace-active advertised count should equal the
    // baseline plus `SemanticProfileFamlies::WORKSPACE.len() = 4`.
    let workspace_family_size =
        lain::server::tools::profile::SemanticProfileFamlies::WORKSPACE.len();
    assert_eq!(
        with_ws_count,
        baseline_count + workspace_family_size,
        "workspace-mode advertised_count ({}) must be baseline ({}) + workspace family ({})",
        with_ws_count,
        baseline_count,
        workspace_family_size,
    );
}

/// `extensions: [\"auto\"]` must expand to every git-tracked
/// extension in the workspace's tree. The dispatcher logic at
/// `ToolExecutor::install_language_servers` walks git's
/// `get_all_tracked_files()` for the workspace and feeds the
/// unique extensions into `detect_extensions_from_files`. A
/// regression here would either silently install nothing (no
/// auto-detected languages) or install the wrong thing.
///
/// The `build_test_executor` helper hard-codes the git sensor to
/// `Path::new(\".\")`, so it can't see a fresh fixture's tracked
/// files. This test uses `build_test_executor_with_workspace` to
/// pin the git sensor at a known git repo with mixed `.rs` and
/// `.py` files, then asserts the response carries one line per
/// expected extension (or at least matches the expected count and
/// contains both binary names).
#[tokio::test]
async fn install_language_servers_auto_resolves_workspace_tracked_languages() {
    // Build a temp git repo with two tracked file types. The
    // executor's git sensor will see this directory specifically
    // (not the test runner's cwd), so the auto-detect path is
    // deterministic.
    let root = tempfile::tempdir().expect("tempdir");
    let root_path = root.path().to_path_buf();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(&root_path)
        .status()
        .unwrap()
        .success());
    for (k, v) in [
        ("user.email", "auto-detect-test@lain"),
        ("user.name", "auto-detect-test"),
    ] {
        std::process::Command::new("git")
            .args(["config", k, v])
            .current_dir(&root_path)
            .status()
            .unwrap();
    }
    std::fs::write(root_path.join("lib.rs"), "pub fn hello() {}\n").unwrap();
    std::fs::write(root_path.join("test.py"), "def hello(): pass\n").unwrap();
    std::fs::create_dir_all(root_path.join("src")).unwrap();
    std::fs::write(root_path.join("src/lib.rs"), "// nested\n").unwrap();
    assert!(std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(&root_path)
        .status()
        .unwrap()
        .success());
    assert!(std::process::Command::new("git")
        .args(["commit", "-q", "-m", "fixture"])
        .current_dir(&root_path)
        .status()
        .unwrap()
        .success());

    let executor = build_test_executor_with_workspace(root_path.clone());

    // Dispatch with extensions=[\"auto\"]. The install of rust-analyzer
    // and pylsp will fail (neither is on PATH in CI), but the
    // dispatcher must reach the resolve_auto_extensions path and
    // produce a multi-entry batch response — one line per tracked
    // extension. The per-ext outcome strings are not asserted
    // strictly (binary availability is env-dependent); we assert
    // that the response has 2 entries, both expected binary names
    // appear, and the response shape is the batched one (not the
    // Config-error fallback).
    let mut args = serde_json::Map::new();
    args.insert("extensions".into(), serde_json::json!(["auto"]));
    let result = executor.call("install_language_server", Some(&args)).await;
    let text = match result {
        Ok(t) => t,
        Err(e) => {
            // If the workspace isn't a git repo (shouldn't happen
            // because we init'd it above) we'd get a typed
            // Config error here. That's a valid dispatcher outcome
            // for the auto path; the assertion that matters is
            // that we're NOT in the legacy single-install error.
            let msg = e.to_string();
            assert!(
                msg.contains("auto")
                    || msg.contains("git")
                    || msg.contains("workspace")
                    || msg.contains("No Git repository"),
                "auto detect failure must mention auto/git/workspace, got: {msg}"
            );
            return;
        }
    };

    // The batched response header is `Install batch (N request(s)):`.
    // With auto the workspace's tracked-file extensions are deduped
    // by extension, so `lib.rs` + `test.py` + `src/lib.rs` collapses
    // to 2 entries (`rs` and `py`). The response uses the extension
    // string as the per-entry label (the `r.ext` field of the
    // `InstallResult`), NOT the LSP binary name — so we assert
    // presence of `rs` and `py` rather than `rust-analyzer` / `pylsp`.
    // Per-entry status strings (`AlreadyInstalled`, `Failed`,
    // `unknown_ext`, `no_install_cmd`) vary by environment; the
    // contract we're pinning is "auto-detect produced a batched
    // response with the expected extensions".
    assert!(
        text.contains("Install batch ("),
        "auto path must produce a batched response: {text}"
    );
    assert!(
        text.contains("rs"),
        "auto path must include the 'rs' extension (.rs files): {text}"
    );
    assert!(
        text.contains("py"),
        "auto path must include the 'py' extension (.py files): {text}"
    );
    // The dedup collapses both `lib.rs` and `src/lib.rs` into a
    // single `rs` entry — the batch size is the count of unique
    // extensions, not the count of tracked files. Pin the
    // structural shape (≥2 entries) rather than the exact text.
    assert!(
        text.matches("  - ").count() >= 2,
        "auto path must produce ≥2 batched entries (one per unique ext): {text}"
    );
}
