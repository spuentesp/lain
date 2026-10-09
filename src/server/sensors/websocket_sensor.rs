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
use crate::graph::{GraphDatabase, SensorOwner};
use crate::schema::{EdgeType, GraphEdge, NodeType};
use crate::server::sensors::patterns::Patterns;
use crate::server::sensors::util;
use std::path::Path;
use std::sync::OnceLock;

/// WebSocket endpoint extracted from code
#[derive(Debug, Clone)]
pub struct WebSocketEndpoint {
    pub url: String,
    pub handler_name: String,
    pub file_path: String,
    pub line: u32,
}

/// Compiled-once regexes for `extract_websocket_patterns`. The four
/// patterns were previously compiled inside the scan loop on every
/// call, which is a real per-file cost — `regex::Regex::new` walks
/// the pattern, runs DFA minimisation, allocates a DFA buffer, and
/// returns an owned `Regex`. Hoisting to `OnceLock` keeps the DFA
/// alive for the process lifetime and is invisible to callers.
///
/// The `expect` on `get_or_init` is unreachable in practice: the
/// pattern strings are static literals, so `Regex::new` cannot fail
/// at runtime. If the patterns are ever made non-literal (a follow-
/// up that parameterises them on `Lang`), the failure should be
/// surfaced through the existing `PatternsError` channel — not
/// through a `OnceLock` panic.
static WS_URL_RE: OnceLock<regex::Regex> = OnceLock::new();
static WS_HANDLER_RE: OnceLock<regex::Regex> = OnceLock::new();
static WS_CTOR_RE: OnceLock<regex::Regex> = OnceLock::new();
static WS_SERVER_RE: OnceLock<regex::Regex> = OnceLock::new();

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

    // The pre-Tier-1 implementation compiled four hardcoded
    // regexes inside this function on every call (Step 4 of Task
    // 8 hoisted them to `OnceLock` statics). Tier 1 (also Task
    // 8) replaces the hardcoded regexes with the generic walker
    // in `util::walk_idioms` and consumes them from
    // `frameworks.yaml`. The four legacy `OnceLock`s are kept
    // as a no-op fallback for callers that bypass the walker
    // (none today — kept for the regression test suite that
    // exercises the regex compilation).
    let _ =
        WS_URL_RE.get_or_init(|| regex::Regex::new(r#""(wss?://[^"']+)""#).expect("ws_url regex"));
    let _ = WS_HANDLER_RE.get_or_init(|| {
        regex::Regex::new(r#"(on(?:open|message|close|error))\s*[=:]\s*(\w+)"#)
            .expect("ws_handler regex")
    });
    let _ = WS_CTOR_RE.get_or_init(|| {
        regex::Regex::new(r#"new\s+WebSocket\s*\(\s*["']([^"']+)["']"#).expect("ws_ctor regex")
    });
    let _ = WS_SERVER_RE.get_or_init(|| {
        regex::Regex::new(r#"(?:app\.ws|router\.ws|WebSocketGateway)\s*\(\s*["']([^"']+)["']"#)
            .expect("ws_server regex")
    });

    // Tier-1: consume the YAML entries via the generic walker.
    // The walker compiles each `path_regex` once and yields a
    // `Vec<IdiomMatch>` per line; the pre-Tier-1 emission shape
    // `(client_url, server_route, handler_name, line)` is
    // preserved.
    let patterns = Patterns::patterns();
    let (clients, servers, handlers) = util::websocket_idioms_for(patterns);

    for (line_no, line) in content.lines().enumerate() {
        let line_num = line_no as u32 + 1;
        // Server routes
        for m in util::walk_idioms(&servers, line, line_num) {
            if let Some(route) = m.literal.as_deref() {
                endpoints.push((String::new(), route.to_string(), String::new(), line_num));
            }
        }
        // Client URLs (both URL-literal and `new WebSocket(…)`).
        for m in util::walk_idioms(&clients, line, line_num) {
            if let Some(url) = m.literal.as_deref() {
                endpoints.push((url.to_string(), String::new(), String::new(), line_num));
            }
        }
        // Event handlers. The walker populates `identifier`
        // with the handler name (group 2 of the YAML's
        // `ws-event-handler` regex); group 1 (the event name
        // `on{open,…}`) is kept in `literal` so the alternation
        // matches the four `on{…}` shapes in a single
        // non-optional branch.
        for m in util::walk_idioms(&handlers, line, line_num) {
            if let Some(handler_name) = m.identifier.as_deref() {
                endpoints.push((
                    String::new(),
                    String::new(),
                    handler_name.to_string(),
                    line_num,
                ));
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
            let key = format!(
                "{}{}:{}",
                crate::server::sensors::util::WS_CLIENT_PREFIX,
                host,
                route
            );
            if !seen_keys.insert(key.clone()) {
                continue;
            }

            let mut node = crate::server::sensors::util::site_node(
                NodeType::HttpClientCall,
                key,
                &node_path,
                None,
                line,
                namespace,
            );
            node.contract = vec![ContractFact::WebSocketConsumer(WebSocketConsumerFact {
                url: NormalizedUrl {
                    host: if host.is_empty() {
                        HostPart::None
                    } else {
                        HostPart::Literal(host)
                    },
                    template: Some(route.clone()),
                },
                route,
            })];
            graph.upsert_node(node)?;
            count += 1;
        } else if !server_route.is_empty() {
            // Server route -> ProviderFact
            let route = if server_route.starts_with('/') {
                server_route
            } else {
                format!("/{server_route}")
            };
            let key = format!(
                "{}{}",
                crate::server::sensors::util::WS_SERVER_PREFIX,
                route
            );
            if !seen_keys.insert(key.clone()) {
                continue;
            }

            let mut node = crate::server::sensors::util::site_node(
                NodeType::HttpRoute,
                key,
                &node_path,
                None,
                line,
                namespace,
            );
            node.contract = vec![ContractFact::WebSocketProvider(WebSocketProviderFact {
                route,
                handler: None,
            })];
            graph.upsert_node(node)?;
            count += 1;
        } else if !handler_name.is_empty() {
            // Server handler -> ProviderFact + Uses edge
            let route = format!("/{}", handler_name);
            let key = format!("ws:handler:{}", handler_name);
            if !seen_keys.insert(key.clone()) {
                continue;
            }

            let mut node = crate::server::sensors::util::site_node(
                NodeType::HttpRoute,
                key,
                &node_path,
                None,
                line,
                namespace,
            );
            let node_id = node.id.clone();
            node.signature = Some(handler_name.clone());
            let repo_id = crate::federation::repo_id::RepoId::new(
                root.file_name().and_then(|f| f.to_str()).unwrap_or("repo"),
            )
            .unwrap_or_else(|_| crate::server::sensors::util::fallback_repo_id());
            node.contract = vec![ContractFact::WebSocketProvider(WebSocketProviderFact {
                route,
                handler: Some(SymbolKey {
                    repo: repo_id,
                    path: node_path.clone(),
                    container: None,
                    name: handler_name.clone(),
                }),
            })];
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

    if graph.is_read_only() {
        return Ok(count);
    }
    // Retract this sensor's previous output first, then upsert the
    // current set below. Without this, nodes are only ever added: a
    // route that moved or disappeared left its stale provider in the
    // graph forever, because `sensor_owner_of` hands these nodes to
    // `SensorOwner::WebSocketSensor` and nobody was calling
    // `replace_sensor_output` with it. Same shape as
    // `entry_point_sensor`, which also resets with empty slices.
    graph.replace_sensor_output(SensorOwner::WebSocketSensor, &[], &[] as &[GraphEdge])?;

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
            if let Some(ContractFact::WebSocketConsumer(c)) = node.contract.first() {
                if c.route == "/events/stream" {
                    found_consumer = true;
                }
            }
            if let Some(ContractFact::WebSocketProvider(p)) = node.contract.first() {
                if p.route == "/events/stream" {
                    found_provider = true;
                }
            }
        }

        assert!(found_consumer, "must emit WebSocketConsumerFact");
        assert!(found_provider, "must emit WebSocketProviderFact");
    }
}
