//! Regression for an index_map corruption in `replace_nodes_for_paths`.
//!
//! Graph node ids are deterministic (UUID5 over `type + path + name`),
//! so a re-scan of the same path inserts a node with the same id. The
//! pre-fix implementation removed the stale id from `index_map` in a
//! deferred pass *after* inserting the replacement, wiping the fresh
//! entry. Every id-keyed read — `get_node`, `get_edges_to`, and the
//! blast-radius BFS — then resolved to nothing while name-keyed reads
//! (`find_node_by_name`, `find_anchors`) still appeared to work. Live
//! symptom: blast radius returned "(no dependents)" for symbols whose
//! edges existed in the on-disk graph (load rebuilds the index) but
//! not in the in-memory one (post-replace).
//!
//! This test pins the contract: after re-scanning the same path, an
//! id lookup must still resolve the node, and `find_node_by_name` must
//! agree. (Incident edges are removed with the old `NodeIndex` and
//! must be re-inserted by the scanner — `replace_nodes_for_paths` is
//! not responsible for reattaching them.)

use lain::graph::GraphDatabase;
use lain::schema::{EdgeType, GraphEdge, GraphNode, NodeType};

fn make_node(kind: NodeType, name: &str, path: &str) -> GraphNode {
    GraphNode::new(kind, name.to_string(), path.to_string())
}

#[test]
fn replace_nodes_for_paths_preserves_index_map_for_deterministic_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let db = GraphDatabase::new(&tmp.path().join("g.bin")).unwrap();

    let compute = make_node(NodeType::Function, "compute", "/src/compute.rs");
    db.upsert_node(compute.clone()).unwrap();

    // Re-scan the same path; deterministic id collides with the first.
    let compute2 = make_node(NodeType::Function, "compute", "/src/compute.rs");
    db.replace_nodes_for_paths(&[compute2.path.clone()], &[compute2.clone()])
        .unwrap();

    // Id-keyed lookup must resolve the (new) node. Pre-fix this returned
    // `None` because the deferred removal wiped the fresh index entry.
    let looked_up = db
        .get_node(&compute2.id)
        .expect("id lookup")
        .expect("id must still resolve after a deterministic-id replace");
    assert_eq!(looked_up.id, compute2.id);

    // `find_node_by_name` (scans node weights) must agree on the id
    // it hands back — confirms the id-keyed view and the name-keyed
    // view describe the same node.
    assert_eq!(
        db.find_node_by_name("compute").map(|n| n.id),
        Some(compute2.id.clone())
    );

    // Sanity: an unrelated edge inserted *after* the replace is still
    // inserted correctly, proving the index_map is functional (the
    // pre-fix state would have rejected the edge with NotFound).
    let caller = make_node(NodeType::Function, "main", "/src/main.rs");
    db.upsert_node(caller.clone()).unwrap();
    db.insert_edge(&GraphEdge::new(
        EdgeType::Calls,
        caller.id.clone(),
        compute2.id.clone(),
    ))
    .unwrap();
    assert!(
        db.get_edges_to(&compute2.id)
            .unwrap()
            .iter()
            .any(|e| matches!(e.edge_type, EdgeType::Calls) && e.source_id == caller.id),
        "edges inserted after the replace must land"
    );
}
