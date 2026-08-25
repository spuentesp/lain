//! The volatile overlay is what makes lain's answers current between
//! reindexes: the file watcher parses a changed file and inserts its
//! symbols here, and every tool resolves names through the overlay before
//! falling back to the static graph.
//!
//! The test this file used to hold was named `overlay_freshness_after_touch`
//! and touched nothing. It built an empty `VolatileOverlay`, asserted that
//! a just-constructed object was less than five seconds old — true by
//! construction, for any overlay, forever — and checked that the blast
//! radius output contained the string "live". No file was written, no
//! watcher ran, `process_file` was never called. It passed identically
//! whether the overlay worked, was empty, or was never written to by
//! anything, which is exactly what happened: `FileWatcher` had no caller
//! in production and the overlay stayed empty in every running server
//! while this test stayed green.

use lain::overlay::VolatileOverlay;
use lain::schema::{GraphEdge, GraphNode, EdgeType, NodeType};
use lain::server::tools::handlers::impact::get_blast_radius;

fn graph(tag: &str) -> lain::graph::GraphDatabase {
    let path = std::env::temp_dir().join(format!("test_freshness_{tag}.bin"));
    let _ = std::fs::remove_dir_all(&path);
    lain::graph::GraphDatabase::new(&path).unwrap()
}

/// The promise: a symbol the watcher has seen but that is not yet in the
/// static graph is still answerable. This is the whole point of the
/// overlay — without it, every answer is only as current as the last
/// commit and reindex.
#[tokio::test]
async fn a_symbol_known_only_to_the_overlay_is_resolvable() {
    let g = graph("overlay_only");
    let overlay = VolatileOverlay::new();

    // Nothing in the static graph knows this symbol.
    assert!(
        g.find_all_nodes_by_name("freshly_edited_fn").is_empty(),
        "precondition: the static graph must not contain the new symbol"
    );

    // What `process_file` does when the watcher sees a save.
    let fresh = GraphNode::new(
        NodeType::Function,
        "freshly_edited_fn".into(),
        "/src/edited.rs".into(),
    );
    overlay.insert_node(fresh.clone());

    let out = get_blast_radius(&g, &overlay, "freshly_edited_fn", false, None)
        .await
        .expect("a symbol present in the overlay must resolve");
    assert!(
        out.contains("freshly_edited_fn"),
        "the answer should be about the overlay symbol: {out}"
    );
}

/// The overlay must actually accumulate what is inserted into it. The
/// old test never inserted anything, so an overlay that silently dropped
/// every write would have passed it.
#[tokio::test]
async fn inserted_symbols_are_retained_and_counted() {
    let overlay = VolatileOverlay::new();
    assert_eq!(overlay.stats().node_count, 0, "starts empty");

    for i in 0..3 {
        overlay.insert_node(GraphNode::new(
            NodeType::Function,
            format!("edited_fn_{i}"),
            "/src/edited.rs".into(),
        ));
    }

    assert_eq!(
        overlay.stats().node_count,
        3,
        "every inserted symbol must be retained"
    );
    assert!(overlay.get_node(&GraphNode::new(
        NodeType::Function,
        "edited_fn_1".into(),
        "/src/edited.rs".into(),
    ).id).is_some());
}

/// Freshness must track the last write, not the construction of the
/// object. Asserting `< 5.0` on a brand-new overlay — which is what the
/// old test did — cannot distinguish a live overlay from a dead one.
#[tokio::test]
async fn freshness_reflects_the_last_update_not_construction() {
    let g = graph("freshness_label");
    let overlay = VolatileOverlay::new();

    let node = GraphNode::new(NodeType::Function, "compute".into(), "/src/c.rs".into());
    let caller = GraphNode::new(NodeType::Function, "main".into(), "/src/main.rs".into());
    g.upsert_node(node.clone()).unwrap();
    g.upsert_node(caller.clone()).unwrap();
    g.insert_edge(&GraphEdge::new(EdgeType::Calls, caller.id.clone(), node.id.clone()))
        .unwrap();

    let before = overlay.last_update_age_secs();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let aged = overlay.last_update_age_secs();
    assert!(
        aged > before,
        "age must advance with wall clock: {before} -> {aged}"
    );

    overlay.touch();
    assert!(
        overlay.last_update_age_secs() < aged,
        "a write must reset the freshness clock"
    );

    let out = get_blast_radius(&g, &overlay, "compute", false, None)
        .await
        .unwrap();
    assert!(
        out.contains("live") || out.contains("recent"),
        "a just-touched overlay reports live/recent: {out}"
    );
}
