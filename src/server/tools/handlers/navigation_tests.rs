//! Tests for tools/handlers/navigation.rs
//!
//! The empty-answer contract: "no call path found" must not read as a
//! confirmed absence when the index cannot prove it saw the code.

use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::{GraphNode, NodeType, RepoNamespace};
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

/// A name that matches several definitions makes "no path" an answer
/// about one of them, picked by list order: the report must name the
/// ambiguity rather than read as a confirmed absence about all of them.
#[tokio::test]
async fn a_doubly_defined_name_makes_the_answer_provisional() {
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
        out.contains("ambiguous_target"),
        "a name with several definitions must qualify the no-path answer, got:\n{out}"
    );
}

/// The mirror case: ambiguity on the `to` end qualifies the answer the
/// same way — the path search covers every definition of both names,
/// so a name picked arbitrarily at either end makes "no path" an
/// answer about that pick, not about the name.
#[tokio::test]
async fn a_doubly_defined_target_makes_the_answer_provisional() {
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
        out.contains("ambiguous_target"),
        "a target name with several definitions must qualify the no-path answer, got:\n{out}"
    );
}
