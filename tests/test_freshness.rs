use lain::overlay::VolatileOverlay;
use lain::server::tools::handlers::impact::get_blast_radius;
use lain::schema::{GraphNode, GraphEdge, NodeType, EdgeType};

#[tokio::test]
async fn overlay_freshness_after_touch() {
    let g = lain::graph::GraphDatabase::new(&std::env::temp_dir().join("test_freshness.bin")).unwrap();
    let overlay = VolatileOverlay::new();
    assert!(overlay.last_update_age_secs() < 5.0, "fresh overlay should be < 5s old");

    let main = GraphNode::new(NodeType::Function, "main".into(), "/src/main.rs".into());
    let compute = GraphNode::new(NodeType::Function, "compute".into(), "/src/compute.rs".into());
    g.upsert_node(main.clone()).unwrap();
    g.upsert_node(compute.clone()).unwrap();
    g.insert_edge(&GraphEdge::new(EdgeType::Calls, main.id.clone(), compute.id.clone())).unwrap();

    let out = get_blast_radius(&g, &overlay, "compute", false, None).await.unwrap();
    println!("freshness line: {}", out.lines().find(|l| l.contains("freshness")).unwrap_or("none"));
    assert!(out.contains("live") || out.contains("recent"), "freshness must be live/recent on fresh graph: {}", out);
}
