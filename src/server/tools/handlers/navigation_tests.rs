//! Tests for tools/handlers/navigation.rs
//!
//! The empty-answer contract: "no call path found" must not read as a
//! confirmed absence when the index cannot prove it saw the code.

use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
use crate::server::tools::handlers::navigation::get_call_chain;

fn scanned_node(name: &str, path: &str, ns: &RepoNamespace) -> GraphNode {
    let mut n = GraphNode::new(NodeType::Function, name.to_string(), path.to_string());
    n.id = GraphNode::generate_id(&NodeType::Function, path, name, None, ns);
    n.last_lsp_sync = Some(crate::server::time::unix_secs(std::time::SystemTime::now()));
    n
}

/// Two symbols with no call path between them, in a current index: the
/// answer is complete, so nothing qualifies it.
#[tokio::test]
async fn an_unrelated_pair_is_a_complete_answer() {
    let dir = std::env::temp_dir().join("lain_test_chain_fresh");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(dir.join("src/b.rs"), "fn b() {}\n").unwrap();
    let graph = GraphDatabase::new(&dir.join("graph.bin")).unwrap();
    let overlay = VolatileOverlay::new();
    let ns = RepoNamespace::for_test();
    for (name, path) in [("a", "src/a.rs"), ("b", "src/b.rs")] {
        graph.upsert_node(scanned_node(name, path, &ns)).unwrap();
    }
    let out = get_call_chain(&graph, &overlay, &dir, None, "a", "b", None)
        .await
        .unwrap();
    assert!(
        out.contains("No call path found"),
        "expected the no-path answer, got:\n{out}"
    );
    assert!(
        !out.contains("stale_or_unindexed") && !out.contains("dynamic_target"),
        "a complete answer must carry no gap reason, got:\n{out}"
    );
}

/// A file edited after its scan makes "no path" provisional: the path
/// may exist in code the index has not seen.
#[tokio::test]
async fn a_stale_end_makes_the_answer_provisional() {
    let dir = std::env::temp_dir().join("lain_test_chain_stale");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(dir.join("src/b.rs"), "fn b() {}\n").unwrap();
    let graph = GraphDatabase::new(&dir.join("graph.bin")).unwrap();
    let overlay = VolatileOverlay::new();
    let ns = RepoNamespace::for_test();
    let mut a = scanned_node("a", "src/a.rs", &ns);
    // `a` was scanned an hour ago; its file was written just now.
    a.last_lsp_sync = Some(crate::server::time::unix_secs(std::time::SystemTime::now()) - 3600);
    graph.upsert_node(a).unwrap();
    graph
        .upsert_node(scanned_node("b", "src/b.rs", &ns))
        .unwrap();
    let out = get_call_chain(&graph, &overlay, &dir, None, "a", "b", None)
        .await
        .unwrap();
    assert!(
        out.contains("stale_or_unindexed"),
        "a stale index must qualify the no-path answer, got:\n{out}"
    );
}

/// A symbol reached through a dispatcher is not a conclusive "no path".
#[tokio::test]
async fn a_dynamic_end_makes_the_answer_provisional() {
    let dir = std::env::temp_dir().join("lain_test_chain_dynamic");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    // `a`'s file is a dispatch site: the graph cannot see what that
    // dispatch reaches.
    std::fs::write(
        dir.join("src/a.rs"),
        "trait Handler { fn handle(&self, req: Request); }\nfn run(h: Box<dyn Handler>) { h.handle(req); }\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/b.rs"), "fn b() {}\n").unwrap();
    let graph = GraphDatabase::new(&dir.join("graph.bin")).unwrap();
    let overlay = VolatileOverlay::new();
    let ns = RepoNamespace::for_test();
    crate::server::sensors::dynamic_dispatch_sensor::scan_workspace_dispatch(&graph, &dir, &ns)
        .unwrap();
    for (name, path) in [("a", "src/a.rs"), ("b", "src/b.rs")] {
        graph.upsert_node(scanned_node(name, path, &ns)).unwrap();
    }
    let out = get_call_chain(&graph, &overlay, &dir, None, "a", "b", None)
        .await
        .unwrap();
    assert!(
        out.contains("dynamic_target"),
        "a dynamically-reached symbol is not a conclusive no-path, got:\n{out}"
    );
}

/// Several definitions at either end are not a gap for this tool: the
/// BFS queues every definition of both names, so a "no path" answer is
/// a negative over all of them, and the answer says how many it
/// covered. (The single-target tools' ambiguity label — "this answer
/// covers one of them" — would contradict that sentence.)
#[tokio::test]
async fn a_doubly_defined_name_is_searched_exhaustively() {
    let dir = std::env::temp_dir().join("lain_test_chain_ambiguous");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(dir.join("src/a2.rs"), "fn a() {}\n").unwrap();
    std::fs::write(dir.join("src/b.rs"), "fn b() {}\n").unwrap();
    let graph = GraphDatabase::new(&dir.join("graph.bin")).unwrap();
    let overlay = VolatileOverlay::new();
    let ns = RepoNamespace::for_test();
    for (name, path) in [("a", "src/a.rs"), ("a", "src/a2.rs"), ("b", "src/b.rs")] {
        graph.upsert_node(scanned_node(name, path, &ns)).unwrap();
    }
    let out = get_call_chain(&graph, &overlay, &dir, None, "a", "b", None)
        .await
        .unwrap();
    assert!(
        out.contains("No call path found"),
        "expected the no-path answer, got:\n{out}"
    );
    assert!(
        out.contains("covered every definition of both names (2 for 'a', 1 for 'b')"),
        "the answer must state the exhaustive coverage, got:\n{out}"
    );
    assert!(
        !out.contains("ambiguous_target"),
        "an exhaustive negative must not claim it covers one definition, got:\n{out}"
    );
}

/// The mirror case: several definitions on the `to` end are covered
/// the same exhaustively, and the answer must say so rather than claim
/// it answered about one pick.
#[tokio::test]
async fn a_doubly_defined_target_is_searched_exhaustively() {
    let dir = std::env::temp_dir().join("lain_test_chain_ambiguous_to");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(dir.join("src/b.rs"), "fn b() {}\n").unwrap();
    std::fs::write(dir.join("src/b2.rs"), "fn b() {}\n").unwrap();
    let graph = GraphDatabase::new(&dir.join("graph.bin")).unwrap();
    let overlay = VolatileOverlay::new();
    let ns = RepoNamespace::for_test();
    for (name, path) in [("a", "src/a.rs"), ("b", "src/b.rs"), ("b", "src/b2.rs")] {
        graph.upsert_node(scanned_node(name, path, &ns)).unwrap();
    }
    let out = get_call_chain(&graph, &overlay, &dir, None, "a", "b", None)
        .await
        .unwrap();
    assert!(
        out.contains("No call path found"),
        "expected the no-path answer, got:\n{out}"
    );
    assert!(
        out.contains("covered every definition of both names (1 for 'a', 2 for 'b')"),
        "the answer must state the exhaustive coverage, got:\n{out}"
    );
    assert!(
        !out.contains("ambiguous_target"),
        "an exhaustive negative must not claim it covers one definition, got:\n{out}"
    );
}

/// A heuristic edge pointing at `from` says callers of `from` are
/// invisible — that bears on "who calls from", not on "does a path
/// from → to exist". Only the dispatch hop out of `from`'s own file can
/// hide a path, so a dynamically-reached start must not add a caveat to
/// a path question.
#[tokio::test]
async fn a_dynamically_reached_start_does_not_qualify_the_path_answer() {
    let dir = std::env::temp_dir().join("lain_test_chain_dyn_start");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(dir.join("src/b.rs"), "fn b() {}\n").unwrap();
    let graph = GraphDatabase::new(&dir.join("graph.bin")).unwrap();
    let overlay = VolatileOverlay::new();
    let ns = RepoNamespace::for_test();
    for (name, path) in [("a", "src/a.rs"), ("b", "src/b.rs")] {
        graph.upsert_node(scanned_node(name, path, &ns)).unwrap();
    }
    // `a` is reached through a dispatch: its callers are invisible.
    let hub = scanned_node("Hub:dispatch", "src/hub.rs", &ns);
    graph.upsert_node(hub.clone()).unwrap();
    graph
        .insert_edges_batch(&[GraphEdge::new(
            EdgeType::DynamicDispatch,
            hub.id.clone(),
            graph
                .find_all_nodes_by_name("a")
                .first()
                .expect("node a")
                .id
                .clone(),
        )])
        .unwrap();
    let out = get_call_chain(&graph, &overlay, &dir, None, "a", "b", None)
        .await
        .unwrap();
    assert!(
        out.contains("No call path found"),
        "expected the no-path answer, got:\n{out}"
    );
    assert!(
        !out.contains("dynamic_target"),
        "an invisible caller of the start is not an invisible path, got:\n{out}"
    );
}

/// The form that does bear on the question at the `to` end: `to` is
/// reached through a dispatch, so a path may exist through a hop the
/// static graph cannot follow and "no path" is not conclusive.
#[tokio::test]
async fn a_dynamically_reached_target_qualifies_the_path_answer() {
    let dir = std::env::temp_dir().join("lain_test_chain_dyn_target");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(dir.join("src/b.rs"), "fn b() {}\n").unwrap();
    let graph = GraphDatabase::new(&dir.join("graph.bin")).unwrap();
    let overlay = VolatileOverlay::new();
    let ns = RepoNamespace::for_test();
    for (name, path) in [("a", "src/a.rs"), ("b", "src/b.rs")] {
        graph.upsert_node(scanned_node(name, path, &ns)).unwrap();
    }
    let hub = scanned_node("Hub:dispatch", "src/hub.rs", &ns);
    graph.upsert_node(hub.clone()).unwrap();
    graph
        .insert_edges_batch(&[GraphEdge::new(
            EdgeType::DynamicDispatch,
            hub.id.clone(),
            graph
                .find_all_nodes_by_name("b")
                .first()
                .expect("node b")
                .id
                .clone(),
        )])
        .unwrap();
    let out = get_call_chain(&graph, &overlay, &dir, None, "a", "b", None)
        .await
        .unwrap();
    assert!(
        out.contains("dynamic_target"),
        "a dynamically-reached target makes no-path provisional, got:\n{out}"
    );
}
