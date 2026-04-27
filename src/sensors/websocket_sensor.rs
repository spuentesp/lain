//! WebSocket protocol sensor
//!
//! Detects WebSocket endpoints and event handlers from code patterns.
//! Scans for ws:// URLs, Upgrade headers, and onopen/onmessage/onclose handlers.
//!
//! Edges created: Uses (handler -> WebSocket endpoint)

use crate::graph::GraphDatabase;
use crate::schema::{GraphNode, GraphEdge, NodeType, EdgeType};
use crate::error::LainError;
use std::path::Path;

/// WebSocket endpoint extracted from code
#[derive(Debug, Clone)]
pub struct WebSocketEndpoint {
    pub url: String,
    pub handler_name: String,
    pub file_path: String,
    pub line: u32,
}

/// Extract WebSocket URLs and handlers from content
fn extract_websocket_patterns(content: &str) -> Vec<(String, String, u32)> {
    let mut endpoints = Vec::new();

    let ws_url_re = regex::Regex::new(r#""(wss?://[^"']+)""#).unwrap();
    let handler_re = regex::Regex::new(r#"(on(?:open|message|close|error))\s*[=:]\s*(\w+)"#).unwrap();
    let ctor_re = regex::Regex::new(r#"new\s+WebSocket\s*\(\s*["']([^"']+)["']"#).unwrap();

    for (line_no, line) in content.lines().enumerate() {
        // WebSocket URLs
        for cap in ws_url_re.captures_iter(line) {
            if let Some(url) = cap.get(1) {
                endpoints.push((url.as_str().to_string(), String::new(), line_no as u32 + 1));
            }
        }
        // Constructor URLs
        for cap in ctor_re.captures_iter(line) {
            if let Some(url) = cap.get(1) {
                endpoints.push((url.as_str().to_string(), String::new(), line_no as u32 + 1));
            }
        }
        // Event handlers
        for cap in handler_re.captures_iter(line) {
            let handler_name = cap.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
            if !handler_name.is_empty() {
                endpoints.push((String::new(), handler_name, line_no as u32 + 1));
            }
        }
    }

    endpoints
}

/// Find handler by name in graph
fn find_handler(graph: &GraphDatabase, name: &str) -> Option<GraphNode> {
    if name.is_empty() {
        return None;
    }
    graph.find_node_by_name(name)
}

/// Enrich graph with WebSocket endpoints
pub fn enrich_with_websocket(
    graph: &GraphDatabase,
    file_path: &Path,
) -> Result<usize, LainError> {
    let content = std::fs::read_to_string(file_path)?;
    let patterns = extract_websocket_patterns(&content);

    let mut count = 0;
    let mut seen_urls = std::collections::HashSet::new();

    for (url, handler_name, line) in patterns {
        let display_name = if url.is_empty() {
            format!("ws:handler:{}", handler_name)
        } else {
            url.clone()
        };

        let node_id = GraphNode::generate_id(
            &NodeType::Variable,
            &file_path.to_string_lossy(),
            &display_name,
        );

        let mut node = GraphNode::new(
            NodeType::Variable,
            display_name,
            file_path.to_string_lossy().to_string(),
        );
        node.id = node_id.clone();
        node.line_start = Some(line);
        if !handler_name.is_empty() {
            node.signature = Some(handler_name.clone());
        }
        graph.upsert_node(node)?;

        if !handler_name.is_empty() {
            if let Some(handler) = find_handler(graph, &handler_name) {
                let edge = GraphEdge::new(
                    EdgeType::Uses,
                    handler.id.clone(),
                    node_id,
                );
                graph.insert_edge(&edge)?;
                count += 1;
            }
        }

        if !url.is_empty() && seen_urls.insert(url) {
            count += 1;
        }
    }

    Ok(count)
}

/// Scan workspace for WebSocket patterns
pub fn scan_workspace(
    graph: &GraphDatabase,
    root: &Path,
) -> Result<usize, LainError> {
    let mut count = 0;

    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ["js", "ts", "jsx", "tsx", "py", "go", "rs"].contains(&ext) {
            match enrich_with_websocket(graph, path) {
                Ok(n) => count += n,
                Err(e) => tracing::warn!("Failed to scan {:?}: {}", path, e),
            }
        }
    }

    Ok(count)
}