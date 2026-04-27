//! Tests for tools/handlers/cross_runtime.rs

use crate::tools::handlers::cross_runtime::get_cross_runtime_callers;
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::{GraphNode, NodeType, EdgeType, GraphEdge};

fn make_test_graph() -> (GraphDatabase, VolatileOverlay) {
    let tmp = std::env::temp_dir().join("test_cross_runtime");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    // Create nodes with different edge types
    let handler = GraphNode::new(NodeType::Function, "handle_request".to_string(), "/src/handler.rs".to_string());
    let http_route = GraphNode::new(NodeType::HttpRoute, "GET /api/users".to_string(), "/openapi.yaml".to_string());
    let grpc_service = GraphNode::new(NodeType::Class, "UserService".to_string(), "/proto/user.proto".to_string());

    let handler_id = handler.id.clone();
    graph.upsert_node(handler.clone()).unwrap();
    graph.upsert_node(http_route.clone()).unwrap();
    graph.upsert_node(grpc_service.clone()).unwrap();

    // HTTP route calls handler
    graph.insert_edge(&GraphEdge::new(EdgeType::CallsHttp, http_route.id.clone(), handler_id.clone())).unwrap();
    // gRPC implements edge to handler
    graph.insert_edge(&GraphEdge::new(EdgeType::Implements, grpc_service.id.clone(), handler_id.clone())).unwrap();

    let overlay = VolatileOverlay::new();
    (graph, overlay)
}

#[test]
fn test_get_cross_runtime_callers_existing() {
    let (graph, overlay) = make_test_graph();

    let handler = graph.find_node_by_name("handle_request").unwrap();
    let result = get_cross_runtime_callers(&graph, &overlay, &handler.id);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("Cross-Runtime") || text.contains("handle_request"));
}

#[test]
fn test_get_cross_runtime_callers_not_found() {
    let (graph, overlay) = make_test_graph();

    let result = get_cross_runtime_callers(&graph, &overlay, "nonexistent_node_id");
    assert!(result.is_err());
}