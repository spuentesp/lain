//! Shared test fixtures used by every `tests/*.rs` integration
//! binary. Each test file does `mod common;` (Rust treats
//! `tests/common/` as a shared module folder), then uses the helpers
//! from this file.
//!
//! The pre-existing test files duplicated the same four lines of
//! boilerplate — `tempfile::tempdir` + `GraphDatabase::new` +
//! `VolatileOverlay::new` + (sometimes) `NlpEmbedder::new_with_threads`
//! — in every fixture function. This module gives one place to
//! change the construction and one place to test it.
//!
//! New tests should prefer:
//!
//! ```ignore
//! use crate::common::*;
//! let (g, o) = graph_and_overlay();
//! ```
//!
//! over open-coding the tempdir / new / new pair.
//!
//! Note: this `tests/common/mod.rs` pattern is the canonical Rust
//! fixture-sharing idiom (the folder name has to be `common` to be
//! picked up — `mod common;` in each test file references it).

use lain::graph::GraphDatabase;
use lain::nlp::NlpEmbedder;
use lain::overlay::VolatileOverlay;
use std::sync::{Arc, Mutex};

/// Empty `GraphDatabase` backed by a fresh tempdir + `graph.bin` file
/// inside it. Caller owns the tempdir through the returned
/// `GraphDatabase`'s `persistence_path`.
pub fn empty_graph() -> GraphDatabase {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("graph.bin");
    GraphDatabase::new(&path).unwrap()
}

/// Empty `VolatileOverlay`. Pair with [`empty_graph`] via
/// [`graph_and_overlay`].
pub fn empty_overlay() -> VolatileOverlay {
    VolatileOverlay::new()
}

/// `(empty_graph, empty_overlay)` pair. The common starting point
/// for tests that build a fixture from scratch.
pub fn graph_and_overlay() -> (GraphDatabase, VolatileOverlay) {
    (empty_graph(), empty_overlay())
}

/// The canonical call-graph fixture used by `tests/graph_invariants.rs`
/// and a handful of other tests. Shape:
///
/// ```text
/// main -> a -> b -> c   (b has two callers)
/// main -> x -> b
/// main -> y             (y is dead — no outgoing edges)
/// ```
///
/// Nodes: `main`, `a`, `b`, `c`, `x`, `y` (Functions); the matching
/// `File` and `Module`/`Namespace` nodes that the scanner would
/// produce are NOT included — callers add them as needed. `b` is
/// the only function with multiple callers (a + x), making it the
/// canonical "duplicate incoming edge" test target.
pub fn call_graph_fixture() -> (GraphDatabase, VolatileOverlay) {
    use lain::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
    let (g, o) = graph_and_overlay();
    let main = GraphNode::new(NodeType::Function, "main".into(), "/src/main.rs".into());
    let a = GraphNode::new(NodeType::Function, "a".into(), "/src/a.rs".into());
    let b = GraphNode::new(NodeType::Function, "b".into(), "/src/b.rs".into());
    let c = GraphNode::new(NodeType::Function, "c".into(), "/src/c.rs".into());
    let x = GraphNode::new(NodeType::Function, "x".into(), "/src/x.rs".into());
    let y = GraphNode::new(NodeType::Function, "y".into(), "/src/y.rs".into());
    for n in [&main, &a, &b, &c, &x, &y] {
        g.upsert_node(n.clone()).unwrap();
    }
    let edges = [
        GraphEdge::new(EdgeType::Calls, main.id.clone(), a.id.clone()),
        GraphEdge::new(EdgeType::Calls, a.id.clone(), b.id.clone()),
        GraphEdge::new(EdgeType::Calls, b.id.clone(), c.id.clone()),
        GraphEdge::new(EdgeType::Calls, main.id.clone(), x.id.clone()),
        GraphEdge::new(EdgeType::Calls, x.id.clone(), b.id.clone()),
    ];
    for e in &edges {
        g.insert_edge(e).unwrap();
    }
    (g, o)
}

/// Stub NLP embedder + matching cache. Used by tests that need a
/// `ToolContext` without pulling in a real ONNX model.
pub fn stub_embedder_and_cache() -> (
    NlpEmbedder,
    Arc<Mutex<std::collections::HashMap<String, Vec<f32>>>>,
) {
    use lain::nlp::CrossEncoder;
    let _ = CrossEncoder::from_dir(std::path::Path::new("/nonexistent"));
    let embedder = NlpEmbedder::new_with_threads(0).expect("stub embedder");
    let cache = Arc::new(Mutex::new(std::collections::HashMap::new()));
    (embedder, cache)
}

/// Smoke test: every helper returns a usable value and the fixture
/// is internally consistent (the `b` node has two incoming `Calls`
/// edges from `a` and `x`).
#[test]
fn helpers_return_usable_fixtures() {
    let (g, _) = call_graph_fixture();
    let main = g.find_node_by_name("main").expect("main node");
    assert_eq!(main.name, "main");
    let b = g.find_node_by_name("b").expect("b node");
    let incoming = g.get_edges_to(&b.id).unwrap();
    let callers: Vec<_> = incoming
        .iter()
        .filter(|e| matches!(e.edge_type, lain::schema::EdgeType::Calls))
        .map(|e| e.source_id.as_str())
        .collect();
    assert_eq!(callers.len(), 2, "b should have exactly two callers");
}
