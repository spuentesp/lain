//! WebSocket protocol sensor
//!
//! Detects WebSocket endpoints and event handlers from code patterns.
//! Scans for ws:// URLs, Upgrade headers, and onopen/onmessage/onclose handlers.
//!
//! Edges created: Uses (handler -> WebSocket endpoint)

use crate::error::LainError;
use crate::federation::contracts::model::{
    ContractFact, HostPart, NormalizedUrl, SymbolKey, WebSocketConsumerFact, WebSocketProviderFact,
};
use crate::graph::GraphDatabase;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use std::path::Path;

/// WebSocket endpoint extracted from code
#[derive(Debug, Clone)]
pub struct WebSocketEndpoint {
    pub url: String,
    pub handler_name: String,
    pub file_path: String,
    pub line: u32,
}

fn parse_url_path_and_host(url: &str) -> (String, String) {
    if let Some(pos) = url.find("://") {
        let after_scheme = &url[pos + 3..];
        let (host, path) = match after_scheme.find('/') {
            Some(slash) => (&after_scheme[..slash], &after_scheme[slash..]),
            None => (after_scheme, "/"),
        };
        (host.to_string(), path.to_string())
    } else {
        (
            String::new(),
            if url.starts_with('/') {
                url.to_string()
            } else {
                format!("/{url}")
            },
        )
    }
}

/// Extract WebSocket URLs, server routes, and handlers from content
fn extract_websocket_patterns(content: &str) -> Vec<(String, String, String, u32)> {
    // (client_url, server_route, handler_name, line)
    let mut endpoints = Vec::new();

    let ws_url_re = regex::Regex::new(r#""(wss?://[^"']+)""#).unwrap();
    let handler_re =
        regex::Regex::new(r#"(on(?:open|message|close|error))\s*[=:]\s*(\w+)"#).unwrap();
    let ctor_re = regex::Regex::new(r#"new\s+WebSocket\s*\(\s*["']([^"']+)["']"#).unwrap();
    let server_re =
        regex::Regex::new(r#"(?:app\.ws|router\.ws|WebSocketGateway)\s*\(\s*["']([^"']+)["']"#)
            .unwrap();

    for (line_no, line) in content.lines().enumerate() {
        let line_num = line_no as u32 + 1;
        // Server routes
        for cap in server_re.captures_iter(line) {
            if let Some(route) = cap.get(1) {
                endpoints.push((
                    String::new(),
                    route.as_str().to_string(),
                    String::new(),
                    line_num,
                ));
            }
        }
        // Client constructor URLs
        for cap in ctor_re.captures_iter(line) {
            if let Some(url) = cap.get(1) {
                endpoints.push((
                    url.as_str().to_string(),
                    String::new(),
                    String::new(),
                    line_num,
                ));
            }
        }
        // WebSocket URLs in string literals
        for cap in ws_url_re.captures_iter(line) {
            if let Some(url) = cap.get(1) {
                endpoints.push((
                    url.as_str().to_string(),
                    String::new(),
                    String::new(),
                    line_num,
                ));
            }
        }
        // Event handlers
        for cap in handler_re.captures_iter(line) {
            let handler_name = cap
                .get(2)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            if !handler_name.is_empty() {
                endpoints.push((String::new(), String::new(), handler_name, line_num));
            }
        }
    }

    endpoints
}

/// Enrich graph with WebSocket endpoints
pub fn enrich_with_websocket(
    graph: &GraphDatabase,
    file_path: &Path,
    root: &Path,
    namespace: &crate::schema::RepoNamespace,
) -> Result<usize, LainError> {
    if graph.is_read_only() {
        return Ok(0);
    }
    let node_path = crate::graph::graph_path(root, file_path);
    let content = std::fs::read_to_string(file_path)?;
    let patterns = extract_websocket_patterns(&content);

    let mut count = 0;
    let mut seen_keys = std::collections::HashSet::new();

    for (url, server_route, handler_name, line) in patterns {
        if !url.is_empty() {
            // Client dial -> ConsumerFact
            let (host, route) = parse_url_path_and_host(&url);
            let key = format!("ws:client:{}:{}", host, route);
            if !seen_keys.insert(key.clone()) {
                continue;
            }

            let node_id = GraphNode::generate_id(
                &NodeType::HttpClientCall,
                &node_path,
                &key,
                None,
                namespace,
            );
            let mut node = GraphNode::new(NodeType::HttpClientCall, key, node_path.clone());
            node.id = node_id.clone();
            node.line_start = Some(line);
            node.contract = Some(ContractFact::WebSocketConsumer(WebSocketConsumerFact {
                url: NormalizedUrl {
                    host: if host.is_empty() {
                        HostPart::None
                    } else {
                        HostPart::Literal(host)
                    },
                    template: Some(route.clone()),
                },
                route,
            }));
            graph.upsert_node(node)?;
            count += 1;
        } else if !server_route.is_empty() {
            // Server route -> ProviderFact
            let route = if server_route.starts_with('/') {
                server_route
            } else {
                format!("/{server_route}")
            };
            let key = format!("ws:server:{}", route);
            if !seen_keys.insert(key.clone()) {
                continue;
            }

            let node_id =
                GraphNode::generate_id(&NodeType::HttpRoute, &node_path, &key, None, namespace);
            let mut node = GraphNode::new(NodeType::HttpRoute, key, node_path.clone());
            node.id = node_id.clone();
            node.line_start = Some(line);
            node.contract = Some(ContractFact::WebSocketProvider(WebSocketProviderFact {
                route,
                handler: None,
            }));
            graph.upsert_node(node)?;
            count += 1;
        } else if !handler_name.is_empty() {
            // Server handler -> ProviderFact + Uses edge
            let route = format!("/{}", handler_name);
            let key = format!("ws:handler:{}", handler_name);
            if !seen_keys.insert(key.clone()) {
                continue;
            }

            let node_id =
                GraphNode::generate_id(&NodeType::HttpRoute, &node_path, &key, None, namespace);
            let mut node = GraphNode::new(NodeType::HttpRoute, key, node_path.clone());
            node.id = node_id.clone();
            node.line_start = Some(line);
            node.signature = Some(handler_name.clone());
            let repo_id = crate::federation::repo_id::RepoId::new(
                root.file_name().and_then(|f| f.to_str()).unwrap_or("repo"),
            )
            .unwrap_or_else(|_| crate::server::sensors::util::fallback_repo_id());
            node.contract = Some(ContractFact::WebSocketProvider(WebSocketProviderFact {
                route,
                handler: Some(SymbolKey {
                    repo: repo_id,
                    path: node_path.clone(),
                    container: None,
                    name: handler_name.clone(),
                }),
            }));
            graph.upsert_node(node)?;

            if let Some(handler) =
                crate::server::sensors::util::find_handler_in_graph(graph, &handler_name)
            {
                let edge = GraphEdge::new(EdgeType::Uses, handler.id.clone(), node_id);
                graph.insert_edge(&edge)?;
            }
            count += 1;
        }
    }

    Ok(count)
}

/// Scan workspace for WebSocket patterns
pub fn scan_workspace(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &crate::schema::RepoNamespace,
) -> Result<usize, LainError> {
    let mut count = 0;

    for entry in crate::server::sensors::util::walk_workspace(root) {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ["js", "ts", "jsx", "tsx", "py", "go", "rs"].contains(&ext) {
            match enrich_with_websocket(graph, path, root, namespace) {
                Ok(n) => count += n,
                Err(e) => tracing::warn!("Failed to scan {:?}: {}", path, e),
            }
        }
    }

    Ok(count)
}

/// Unit-struct Sensor impl. Discovery via
/// `inventory::submit!(SensorEntry(&WebSocketSensor))` below; no central
/// registry to edit.
pub struct WebSocketSensor;

crate::server::sensors::register_sensor!(WebSocketSensor, "websocket", Websocket, scan_workspace);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::RepoNamespace;
    use tempfile::tempdir;

    #[test]
    fn websocket_sensor_extracts_provider_and_consumer_facts() {
        let tmp = tempdir().unwrap();
        let db_dir = tmp.path().join("db");
        let ws_dir = tmp.path().join("ws_repo");
        std::fs::create_dir_all(&ws_dir).unwrap();

        let client_file = ws_dir.join("client.js");
        std::fs::write(
            &client_file,
            r#"
            const ws = new WebSocket("ws://orders/events/stream");
            ws.onmessage = handleStream;
            "#,
        )
        .unwrap();

        let server_file = ws_dir.join("server.js");
        std::fs::write(
            &server_file,
            r#"
            app.ws("/events/stream", handleStream);
            function handleStream(msg) {}
            "#,
        )
        .unwrap();

        let graph = GraphDatabase::new(&db_dir).unwrap();
        let ns = RepoNamespace::for_test();

        let client_count = enrich_with_websocket(&graph, &client_file, &ws_dir, &ns).unwrap();
        assert!(client_count >= 1);

        let server_count = enrich_with_websocket(&graph, &server_file, &ws_dir, &ns).unwrap();
        assert!(server_count >= 1);

        let mut found_consumer = false;
        let mut found_provider = false;

        for node in graph.all_nodes() {
            if let Some(ContractFact::WebSocketConsumer(c)) = &node.contract {
                if c.route == "/events/stream" {
                    found_consumer = true;
                }
            }
            if let Some(ContractFact::WebSocketProvider(p)) = &node.contract {
                if p.route == "/events/stream" {
                    found_provider = true;
                }
            }
        }

        assert!(found_consumer, "must emit WebSocketConsumerFact");
        assert!(found_provider, "must emit WebSocketProviderFact");
    }
}
