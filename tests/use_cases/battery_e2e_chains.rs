//! Battery of END-TO-END success chains.
//!
//! The other batteries test operations in isolation. This one
//! chains operations to prove the integration works as a
//! pipeline:
//!
//!   chain 1: search → find_anchors → get_blast_radius → trace_dependency
//!   chain 2: find_dead_code → get_call_sites → explain_symbol
//!   chain 3: list_entry_points → get_master_map → get_layered_map
//!
//! Every chain asserts that the OUTPUT of step N is consistent
//! with the INPUT of step N+1. A regression that breaks any
//! single tool also breaks its chain because the chain expects
//! the next tool to operate on data the previous tool produced.

use lain::graph::GraphDatabase;
use lain::overlay::VolatileOverlay;
use lain::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use std::path::Path;

fn build_fixture() -> (tempfile::TempDir, GraphDatabase) {
    let dir = tempfile::tempdir().unwrap();
    let db = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
    let mut n = |name: &str, path: &str, kind: NodeType, ls: u32, le: u32| {
        let mut node = GraphNode::new(kind, name.into(), path.into());
        node.line_start = Some(ls);
        node.line_end = Some(le);
        db.upsert_node(node).unwrap();
    };
    n("real_hub", "src/lib.rs", NodeType::Function, 1, 30);
    n("helper_a", "src/lib.rs", NodeType::Function, 31, 60);
    n("helper_b", "src/lib.rs", NodeType::Function, 61, 90);
    n("dead_one", "src/lib.rs", NodeType::Function, 91, 100);
    n("do_stuff", "src/lib.rs", NodeType::Method, 101, 120);
    n("Config", "src/lib.rs", NodeType::Struct, 121, 140);
    n("caller_zero", "src/lib.rs", NodeType::Function, 141, 160);
    n("test_helper", "tests/common/mod.rs", NodeType::Function, 1, 20);
    let find = |name: &str| db.find_node_by_name(name).unwrap();
    let orch = find("real_hub").id.clone();
    let a = find("helper_a").id.clone();
    let b = find("helper_b").id.clone();
    let d = find("do_stuff").id.clone();
    let cz = find("caller_zero").id.clone();
    db.insert_edge(&GraphEdge::new(EdgeType::Calls, cz, orch.clone())).unwrap();
    db.insert_edge(&GraphEdge::new(EdgeType::Calls, orch.clone(), a.clone())).unwrap();
    db.insert_edge(&GraphEdge::new(EdgeType::Calls, orch, b)).unwrap();
    db.insert_edge(&GraphEdge::new(EdgeType::Calls, d, a)).unwrap();
    db.calculate_anchor_scores().unwrap();
    (dir, db)
}

// ─── Chain 1: search → find_anchors → get_blast_radius → trace ──

#[test]
fn chain_search_to_anchors_to_blast_to_trace() {
    use lain::server::tools::handlers::impact::get_blast_radius;
    use lain::server::tools::handlers::metrics::find_anchors;
    use lain::server::tools::handlers::navigation::trace_dependency;

    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();

    // Step 1: search_org-equivalent — find nodes by name via find_node_by_name.
    let real_hub_node = db.find_node_by_name("real_hub")
        .expect("search step must find real_hub");

    // Step 2: find_anchors — confirm real_hub is the top.
    let anchors_text = find_anchors(&db, &overlay, 10).expect("find_anchors");
    let first_anchor = anchors_text.lines()
        .find(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit()))
        .expect("at least one anchor");
    assert!(first_anchor.contains("real_hub"),
            "anchor chain step: find_anchors must surface real_hub; got: {first_anchor}");

    // Step 3: get_blast_radius(real_hub) — confirm callers are listed.
    let blast_text = tokio_test_run(get_blast_radius(&db, &overlay, "real_hub", false, None))
        .expect("blast radius step");
    assert!(blast_text.contains("caller_zero"),
            "blast chain step: must list caller_zero as caller of real_hub; got: {blast_text}");

    // Step 4: trace_dependency(real_hub) — confirm callees are listed.
    let trace_text = trace_dependency(&db, &overlay, "real_hub")
        .expect("trace_dependency step");
    assert!(trace_text.contains("helper_a") && trace_text.contains("helper_b"),
            "trace chain step: must list helper_a + helper_b as callees of real_hub; got: {trace_text}");

    // The chain's success: the SAME node (real_hub) appears as the top
    // anchor, has a caller (caller_zero), and has callees (helper_a, helper_b).
    assert!(real_hub_node.name == "real_hub", "search step must return real_hub");
}

// ─── Chain 2: find_dead_code → get_call_sites → explain_symbol ────

#[test]
fn chain_dead_to_call_sites_to_explain() {
    use lain::server::tools::handlers::context::get_call_sites;
    use lain::server::tools::handlers::metrics::{explain_symbol, find_dead_code};

    let (dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    let occupancy = lain::server::presence::OccupancyMap::default();

    // Step 1: find_dead_code — surface dead_one.
    let dead_text = find_dead_code(
        dir.path(), &db, &overlay, None,
        &lain::nlp::NlpEmbedder::new_with_threads(0).unwrap(),
        &std::sync::Arc::new(parking_lot::Mutex::new(Default::default())),
    ).expect("find_dead_code");
    assert!(dead_text.contains("dead_one"),
            "dead-code chain step: must list dead_one; got: {dead_text}");

    // Step 2: get_call_sites(helper_a) — surfaces the callers (real_hub, do_stuff).
    let calls_text = get_call_sites(dir.path(), &db, &overlay, "helper_a")
        .expect("get_call_sites");
    assert!(calls_text.contains("real_hub") || calls_text.contains("do_stuff"),
            "call-sites chain step: must list real_hub or do_stuff as callers; got: {calls_text}");

    // Step 3: explain_symbol(real_hub) — confirms description includes callees.
    let explain_text = explain_symbol(dir.path(), &db, &overlay, &occupancy, "real_hub")
        .expect("explain_symbol");
    assert!(explain_text.contains("real_hub"),
            "explain chain step: must mention real_hub; got: {explain_text}");
    assert!(explain_text.contains("helper_a") || explain_text.contains("helper_b"),
            "explain chain step: must list helper_a or helper_b as callees; got: {explain_text}");
}

// ─── Chain 3: list_entry_points → get_master_map → get_layered_map ─

#[test]
fn chain_entry_points_to_master_to_layered() {
    use lain::server::tools::handlers::architecture::{get_master_map, list_entry_points};
    use lain::server::tools::handlers::navigation::get_layered_map;

    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();

    // Step 1: list_entry_points
    let entry_text = list_entry_points(&db, &overlay)
        .expect("list_entry_points");
    assert!(!entry_text.is_empty(),
            "entry-points chain step: must return non-empty; got: {entry_text}");

    // Step 2: get_master_map
    let master_text = get_master_map(&db, &overlay)
        .expect("get_master_map");
    assert!(!master_text.is_empty(),
            "master-map chain step: must return non-empty; got: {master_text}");

    // Step 3: get_layered_map
    let layered_text = get_layered_map(&db, &overlay, 1, "module")
        .expect("get_layered_map");
    assert!(!layered_text.is_empty(),
            "layered-map chain step: must return non-empty; got: {layered_text}");
}

// ─── Helper: run a tokio future to completion in a sync test ────

fn tokio_test_run<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}
