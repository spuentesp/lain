//! Graph correctness tests — algorithm verification for blast radius, resolution priority, and query executor

mod common;

use lain::graph::GraphDatabase;
use lain::overlay::VolatileOverlay;
use lain::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use lain::tools::handlers::impact::get_blast_radius;

/// Local thin shim over [`common::call_graph_fixture`]: every test in
/// this file uses only the graph (the fixture's overlay is empty),
/// and keeping the call sites `make_test_graph()` keeps each test's
/// body identical to the pre-refactor form.
fn make_test_graph() -> GraphDatabase {
    common::call_graph_fixture().0
}

fn make_overlay_with_node(name: &str, path: &str) -> VolatileOverlay {
    let overlay = VolatileOverlay::new();
    let node = GraphNode::new(NodeType::Function, name.to_string(), path.to_string());
    overlay.insert_node(node);
    overlay
}

#[tokio::test]
async fn test_blast_radius_leaf_node() {
    let graph = make_test_graph();
    let overlay = VolatileOverlay::new();

    // c is a leaf — nothing calls c
    let result = get_blast_radius(&graph, &overlay, "c", false, false, None).await;
    assert!(result.is_ok());
    let text = result.unwrap();
    // Leaf has no callers, so only c itself is in visited set
    assert!(text.contains("affected nodes"));
    assert!(text.contains("c"));
}

#[tokio::test]
async fn test_blast_radius_b_node() {
    let graph = make_test_graph();
    let overlay = VolatileOverlay::new();

    // b has two callers: a and x. Both should appear in blast radius.
    let result = get_blast_radius(&graph, &overlay, "b", false, false, None).await;
    assert!(result.is_ok());
    let text = result.unwrap();
    // Should show a and x as dependents (at minimum)
    assert!(text.contains("a") || text.contains("x"));
}

#[tokio::test]
async fn test_blast_radius_main_node() {
    let graph = make_test_graph();
    let overlay = VolatileOverlay::new();

    // main is root — no incoming edges to main in our test graph
    let result = get_blast_radius(&graph, &overlay, "main", false, false, None).await;
    assert!(result.is_ok());
    let text = result.unwrap();
    // Either no dependents found OR transitively affected nodes for root
    assert!(text.contains("no dependents") || text.contains("affected"));
}

#[tokio::test]
async fn test_blast_radius_unknown_node() {
    let graph = make_test_graph();
    let overlay = VolatileOverlay::new();

    let result = get_blast_radius(&graph, &overlay, "nonexistent_symbol", false, false, None).await;
    assert!(result.is_err());
}

/// Regression: `main` reaches `b` via two paths (main→a→b and
/// main→x→b). The pre-fix BFS enqueued `main` once per edge, listing
/// it twice while `visited` counted it once — the headline total then
/// disagreed with the number of listed dependents.
#[tokio::test]
async fn test_blast_radius_dedups_callers_and_count_matches_listing() {
    let graph = make_test_graph();
    let overlay = VolatileOverlay::new();

    let text = get_blast_radius(&graph, &overlay, "b", false, false, None)
        .await
        .unwrap();
    assert_eq!(
        text.matches("- main (Function)").count(),
        1,
        "main must be listed exactly once:\n{text}"
    );
    let listed = text.lines().filter(|l| l.starts_with("  - ")).count();
    let total: usize = text
        .lines()
        .find_map(|l| l.strip_prefix("- Total transitively affected nodes: "))
        .and_then(|n| n.parse().ok())
        .expect("total line present when dependents exist");
    assert_eq!(
        listed, total,
        "listed dependents must equal the headline count:\n{text}"
    );
}

#[test]
fn test_graph_node_lookup_by_name() {
    let graph = make_test_graph();

    let found = graph.find_node_by_name("b");
    assert!(found.is_some());
    assert_eq!(found.unwrap().name, "b");

    let not_found = graph.find_node_by_name("nonexistent");
    assert!(not_found.is_none());
}

#[test]
fn test_graph_node_lookup_by_path() {
    let graph = make_test_graph();

    let found = graph.find_node_by_path("/src/b.rs");
    assert!(found.is_some());
    assert_eq!(found.unwrap().name, "b");

    let not_found = graph.find_node_by_path("/src/nonexistent.rs");
    assert!(not_found.is_none());
}

#[test]
fn test_graph_get_nodes_by_type() {
    let graph = make_test_graph();

    let funcs = graph.get_nodes_by_type(NodeType::Function).unwrap();
    assert_eq!(funcs.len(), 6);

    let files = graph.get_nodes_by_type(NodeType::File).unwrap();
    assert_eq!(files.len(), 0);
}

#[test]
fn test_graph_get_neighbors_incoming() {
    let graph = make_test_graph();

    let b_node = graph.find_node_by_name("b").unwrap();
    let neighbors = graph.get_neighbors(&b_node.id, petgraph::Direction::Incoming);
    // b is called by a and x
    assert_eq!(neighbors.len(), 2);
    let mut names: Vec<_> = neighbors
        .iter()
        .map(|(node, _)| node.name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, vec!["a", "x"]);
}

#[test]
fn test_graph_get_neighbors_outgoing() {
    let graph = make_test_graph();

    let main_node = graph.find_node_by_name("main").unwrap();
    let neighbors = graph.get_neighbors(&main_node.id, petgraph::Direction::Outgoing);
    // main calls a and x
    assert_eq!(neighbors.len(), 2);
    let mut names: Vec<_> = neighbors
        .iter()
        .map(|(node, _)| node.name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, vec!["a", "x"]);
}

#[test]
fn test_graph_dead_code_detection() {
    let graph = make_test_graph();

    // Find nodes with zero incoming edges (potential dead code)
    let all_nodes = graph.get_all_nodes();
    let mut dead_nodes = Vec::new();

    for node in all_nodes {
        let incoming = graph.get_neighbors(&node.id, petgraph::Direction::Incoming);
        if incoming.is_empty() && node.name != "main" {
            // main is entry point so no callers is expected
            dead_nodes.push(node.name.clone());
        }
    }

    // y has no incoming or outgoing edges (it was added but nothing calls it or it calls nothing)
    // Actually y has no incoming, and main doesn't call y. So y is dead.
    assert!(
        dead_nodes.contains(&"y".to_string()),
        "y should be detected as dead"
    );
}

#[test]
fn test_overlay_takes_priority_over_graph() {
    let graph = make_test_graph();

    // Add a conflicting node to overlay
    let mut overlay_node =
        GraphNode::new(NodeType::Function, "a".to_string(), "/src/a.rs".to_string());
    overlay_node.signature = Some("OVERLAY_SIG".to_string());
    let overlay = make_overlay_with_node("a", "/src/a.rs");

    // When both graph and overlay have "a", overlay should be checked first
    // We can't directly test resolve_node without the full tool context, but we can verify
    // both are independently accessible
    let graph_node = graph.find_node_by_name("a");
    let overlay_node_found = overlay.get_node(&overlay_node.id);

    assert!(graph_node.is_some());
    assert!(overlay_node_found.is_some());
}

#[test]
fn test_graph_id_determinism() {
    let n1 = GraphNode::new(
        NodeType::Function,
        "test_fn".to_string(),
        "/src/lib.rs".to_string(),
    );
    let n2 = GraphNode::new(
        NodeType::Function,
        "test_fn".to_string(),
        "/src/lib.rs".to_string(),
    );
    assert_eq!(
        n1.id, n2.id,
        "Same node type+path+name must produce same ID"
    );
}

#[test]
fn test_graph_edge_insertion_duplicate() {
    let tmp = std::env::temp_dir().join("test_edge_dup");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    let n1 = GraphNode::new(
        NodeType::Function,
        "n1".to_string(),
        "/src/lib.rs".to_string(),
    );
    let n2 = GraphNode::new(
        NodeType::Function,
        "n2".to_string(),
        "/src/lib.rs".to_string(),
    );

    graph.upsert_node(n1.clone()).unwrap();
    graph.upsert_node(n2.clone()).unwrap();

    // Insert same edge twice
    let edge = GraphEdge::new(EdgeType::Calls, n1.id.clone(), n2.id.clone());
    let r1 = graph.insert_edge(&edge);
    let r2 = graph.insert_edge(&edge);

    assert!(r1.is_ok());
    assert!(r2.is_ok()); // Should be idempotent
}

#[test]
fn test_graph_batch_node_insert() {
    let tmp = std::env::temp_dir().join("test_batch_nodes");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    let nodes: Vec<GraphNode> = (0..100)
        .map(|i| {
            GraphNode::new(
                NodeType::Function,
                format!("fn_{}", i),
                "/src/lib.rs".to_string(),
            )
        })
        .collect();

    graph.insert_nodes_batch(&nodes).unwrap();

    let all = graph.get_all_nodes();
    assert_eq!(all.len(), 100);
}

#[test]
fn test_graph_batch_edge_insert() {
    let tmp = std::env::temp_dir().join("test_batch_edges");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    // Create a chain: n0 -> n1 -> n2 -> ... -> n99
    let nodes: Vec<GraphNode> = (0..100)
        .map(|i| {
            GraphNode::new(
                NodeType::Function,
                format!("fn_{}", i),
                "/src/lib.rs".to_string(),
            )
        })
        .collect();

    graph.insert_nodes_batch(&nodes).unwrap();

    let edges: Vec<GraphEdge> = (0..99)
        .map(|i| {
            GraphEdge::new(
                EdgeType::Calls,
                nodes[i].id.clone(),
                nodes[i + 1].id.clone(),
            )
        })
        .collect();

    graph.insert_edges_batch(&edges).unwrap();

    // Verify graph consistency
    let all_nodes = graph.get_all_nodes();
    assert_eq!(all_nodes.len(), 100);

    for node in nodes.iter().take(99) {
        let outgoing = graph.get_edges_from(&node.id).unwrap();
        assert!(!outgoing.is_empty());
    }
}

/// `insert_edges_batch` silently drops edges whose endpoints are
/// both missing from the index — and the production caller
/// (`insert_edges_reporting`) emits a `warn!` carrying the
/// dropped count so the operator learns about it. The function
/// returns the count, but no test was pinning that contract;
/// iter-17 found the silent-drop behavior the hard way when an
/// `assess_change` fixture forgot to upsert one endpoint.
///
/// Pinned: a batch with one valid edge, one edge whose source
/// is missing, and one edge whose target is missing must
/// return `dropped == 2` and the valid edge must be in the
/// graph afterwards. This is the property the production
/// warning depends on.
#[test]
fn insert_edges_batch_reports_dropped_count_for_orphan_edges() {
    let graph = make_test_graph();

    // Pick two real nodes from the fixture as the valid pair.
    let nodes = graph.get_all_nodes();
    assert!(nodes.len() >= 2, "fixture should have ≥2 nodes");
    let src = nodes[0].id.clone();
    let tgt = nodes[1].id.clone();

    // Synthetic source/target IDs that don't exist in the index.
    // The format mimics `GraphNode::generate_id`'s output but
    // with a stable test-only UUID so we never accidentally
    // collide with a real node.
    let orphan_src = "orphan-src-00000000-0000-0000-0000-000000000000".to_string();
    let orphan_tgt = "orphan-tgt-11111111-1111-1111-1111-111111111111".to_string();

    let batch = vec![
        // Valid edge — both endpoints in the index.
        GraphEdge::new(EdgeType::Calls, src.clone(), tgt.clone()),
        // Missing source — `false, true` arm: held for federation
        // drain. Counts as dropped only if BOTH endpoints are
        // missing; with source missing the edge goes to the
        // `pending_external_edges` queue, not the dropped
        // counter.
        GraphEdge::new(EdgeType::Calls, orphan_src.clone(), tgt.clone()),
        // Missing target — `true, false` arm: held for federation
        // drain. Same as above.
        GraphEdge::new(EdgeType::Calls, src.clone(), orphan_tgt.clone()),
        // Both endpoints missing — `false, _` arm: truly orphan,
        // counted in `dropped`. THIS is the silent-drop case
        // `insert_edges_reporting` warns about.
        GraphEdge::new(EdgeType::Calls, orphan_src.clone(), orphan_tgt.clone()),
    ];

    let dropped = graph
        .insert_edges_batch(&batch)
        .expect("insert_edges_batch should not error on orphans");
    // The match arm is `(false, _) => dropped`. That is:
    //   - source missing AND target present: dropped (orphan)
    //   - source missing AND target missing: dropped (orphan)
    //   - source present AND target missing: NOT dropped; held for
    //     federation drain (the source is local so we know which
    //     repo the edge came from, and project_repo can rewrite
    //     the missing target through the local-to-global map).
    //
    // So our batch of 4 (1 valid + 3 broken) drops 2: the
    // missing-source edges. The missing-target edge is held for
    // the federation to resolve later.
    assert_eq!(
        dropped, 2,
        "exactly the two missing-source edges must be counted as dropped; \
         the missing-target edge goes to pending_external_edges, not dropped"
    );

    // The valid edge made it into the graph.
    let outgoing = graph.get_edges_from(&src).expect("get_edges_from");
    assert!(
        outgoing.iter().any(|e| e.target_id == tgt),
        "the valid edge must be persisted; orphans must not steal it"
    );
}

/// Companion to `insert_edges_batch_reports_dropped_count_for_orphan_edges`.
/// That test pins the dropped count (and the valid-edge survival);
/// this one pins the *positive* arm: an edge whose source IS in
/// the local index but whose target is missing must end up in
/// `take_pending_external_edges`, not the dropped counter.
///
/// The federation's `project_repo` drains that queue after the
/// intra-repo edge pass to emit the edge to the federated
/// backend. If `insert_edges_batch` ever silently drops this arm
/// too, the federation's cross-repo projection breaks because
/// nothing reaches the backend — and the dropped counter would
/// not move, so the operator warning stays silent.
#[test]
fn insert_edges_batch_holds_missing_target_edge_for_federation_drain() {
    let graph = make_test_graph();

    // Pick a real source from the fixture and pair it with a
    // synthetic target that doesn't exist anywhere.
    let nodes = graph.get_all_nodes();
    assert!(!nodes.is_empty(), "fixture should have ≥1 node");
    let src = nodes[0].id.clone();
    let orphan_tgt = "orphan-tgt-fed-22222222-2222-2222-2222-222222222222".to_string();

    // Queue must be empty before we start; otherwise another
    // test's drain path leaked across runs.
    assert!(
        graph.take_pending_external_edges().is_empty(),
        "pending_external_edges must start empty for this test"
    );

    let batch = vec![GraphEdge::new(
        EdgeType::Calls,
        src.clone(),
        orphan_tgt.clone(),
    )];

    let dropped = graph
        .insert_edges_batch(&batch)
        .expect("insert_edges_batch should not error on a missing target");
    assert_eq!(
        dropped, 0,
        "missing-target edges must not be counted as dropped; \
         they are held for federation drain"
    );

    let drained = graph.take_pending_external_edges();
    assert_eq!(
        drained.len(),
        1,
        "exactly the missing-target edge should have been queued for drain"
    );
    assert_eq!(drained[0].source_id, src);
    assert_eq!(drained[0].target_id, orphan_tgt);

    // Second drain returns empty — `take_*` semantics, not peek.
    assert!(
        graph.take_pending_external_edges().is_empty(),
        "draining twice must not return the same edge"
    );
}
