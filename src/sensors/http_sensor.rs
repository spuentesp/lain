//! HTTP route sensor
//!
//! Extracts HTTP route definitions via regex-first heuristics.
//! Supported patterns:
//!   - Rust: axum, actix-web, rocket (macro-based routes)
//!   - Python: FastAPI, Flask, Django URL patterns
//!   - TypeScript: Express, Fastify route definitions
//!   - Go: net/http, gin, echo
//!
//! Edges created: CallsHttp (route -> handler function)

use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::{GraphNode, GraphEdge, NodeType, EdgeType};
use crate::error::LainError;
use std::collections::HashMap;

/// A detected HTTP route
#[derive(Debug, Clone)]
pub struct HttpRoute {
    pub method: String,       // GET, POST, etc.
    pub path: String,         // /api/users/:id
    pub handler_path: String, // file path
    pub handler_name: String, // function name
    pub line: u32,
}

/// HTTP route patterns per language
struct RoutePattern {
    method_regex: regex::Regex,
    path_regex: regex::Regex,
    handler_fn_regex: regex::Regex,
}

impl RoutePattern {
    fn new(method_pat: &str, path_pat: &str, handler_pat: &str) -> Self {
        Self {
            method_regex: regex::Regex::new(method_pat).unwrap(),
            path_regex: regex::Regex::new(path_pat).unwrap(),
            handler_fn_regex: regex::Regex::new(handler_pat).unwrap(),
        }
    }

    fn extract<'a>(&self, content: &'a str, file_path: &str) -> Vec<HttpRoute> {
        let mut routes = Vec::new();
        for (line_no, line) in content.lines().enumerate() {

            // Method extraction
            let method = self.method_regex.captures(line)
                .map(|c| c.get(1).map(|m| m.as_str().to_uppercase()).unwrap_or("GET".to_string()))
                .unwrap_or_else(|| "GET".to_string());

            // Path extraction
            let path = self.path_regex.captures(line)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();

            // Handler extraction
            let handler = self.handler_fn_regex.captures(line)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();

            if !path.is_empty() && !handler.is_empty() {
                routes.push(HttpRoute {
                    method: method.clone(),
                    path: path.clone(),
                    handler_path: file_path.to_string(),
                    handler_name: handler,
                    line: line_no as u32 + 1,
                });
            }
        }
        routes
    }
}

/// All supported route patterns
fn get_route_patterns() -> HashMap<&'static str, RoutePattern> {
    let mut patterns = HashMap::new();

    // Rust: axum route! macro
    patterns.insert("rust-axum", RoutePattern::new(
        r"(?i)(get|post|put|delete|patch|options)\s*\(",
        r#"route!\s*\(\s*"([^"]+)""#,
        r#"\.handler\(([^)]+)\)"#,
    ));

    // Rust: Actix-web #[get("/path")]
    patterns.insert("rust-actix", RoutePattern::new(
        r#"#\[(get|post|put|delete|patch|options|head)\s*\("#,
        r#"path\s*=\s*"([^"]+)""#,
        r#"fn\s+(\w+)\s*\("#,
    ));

    // Python: FastAPI @app.get("/path")
    patterns.insert("python-fastapi", RoutePattern::new(
        r"@\w+\.(get|post|put|delete|patch|options)\s*\(",
        r#"@[\w\.]+\.(?:get|post|put|delete|patch|options)\s*\(\s*"([^"]+)""#,
        r"async\s+def\s+(\w+)\s*\("#,
    ));

    // Python: Flask @app.route("/path", methods=['GET'])
    patterns.insert("python-flask", RoutePattern::new(
        r"@(app|blueprint)\.route\s*\(",
        r#"route\s*\(\s*"([^"]+)""#,
        r"def\s+(\w+)\s*\("#,
    ));

    // TypeScript: Express router.get('/path', handler)
    patterns.insert("ts-express", RoutePattern::new(
        r#"\.(get|post|put|delete|patch|options)\s*\("#,
        r#"\.get\s*\(\s*"([^"]+)""#,
        r#"\.get\s*\(\s*"[^"]+"\s*,\s*(\w+)"#,
    ));

    // TypeScript: Fastify
    patterns.insert("ts-fastify", RoutePattern::new(
        r#"(get|post|put|delete|patch)\s*\("#,
        r#"\.get\s*\(\s*"([^"]+)""#,
        r#"handler:\s*(\w+)"#,
    ));

    // Go: net/http
    patterns.insert("go-std", RoutePattern::new(
        r#"(Get|Post|Put|Delete|Handle)\s*\("#,
        r#"HandleFunc\s*\(\s*"([^"]+)""#,
        r#"HandleFunc\s*\([^,]+,\s*(\w+)"#,
    ));

    // Go: Gin router.GET("/path", handler)
    patterns.insert("go-gin", RoutePattern::new(
        r#"(GET|POST|PUT|DELETE|PATCH)\s*\("#,
        r#"r\.(?:GET|POST|PUT|DELETE|PATCH)\s*\(\s*"([^"]+)""#,
        r#"r\.(?:GET|POST|PUT|DELETE|PATCH)\s*\(\s*"[^"]+"\s*,\s*(\w+)"#,
    ));

    patterns
}

/// Scan a file for HTTP routes
pub fn scan_file_for_routes(path: &std::path::Path, content: &str) -> Vec<HttpRoute> {
    let extension = path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let all_patterns = get_route_patterns();
    let applicable: Vec<&RoutePattern> = all_patterns.values().collect();

    let mut all_routes = Vec::new();
    for pattern in applicable {
        let routes = pattern.extract(content, &path.to_string_lossy());
        all_routes.extend(routes);
    }

    // Deduplicate by (method, path)
    let mut seen = std::collections::HashSet::new();
    all_routes.retain(|r| seen.insert((r.method.clone(), r.path.clone())));

    all_routes
}

/// Convert HTTP routes to graph nodes and edges
pub fn routes_to_graph(graph: &GraphDatabase, routes: &[HttpRoute]) -> (Vec<GraphNode>, Vec<GraphEdge>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    for route in routes {
        let node_id = GraphNode::generate_id(&NodeType::HttpRoute, &route.handler_path, &format!("{}:{}", route.method, route.path));

        let mut node = GraphNode::new(
            NodeType::HttpRoute,
            format!("{} {}", route.method, route.path),
            route.handler_path.clone(),
        );
        node.id = node_id.clone();
        node.line_start = Some(route.line);
        node.signature = Some(route.handler_name.clone());

        nodes.push(node);

        if let Some(handler) = graph.find_nodes_by_name(&route.handler_name).ok().and_then(|mut h| h.pop()) {
            edges.push(GraphEdge::new(
                EdgeType::CallsHttp,
                node_id,
                handler.id.clone(),
            ));
        }
    }

    (nodes, edges)
}

/// Scan a directory tree for HTTP routes and add to graph
pub fn scan_workspace_routes(
    graph: &GraphDatabase,
    root: &std::path::Path,
) -> Result<usize, LainError> {
    let mut count = 0;
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        // Only scan code files
        if !["rs", "py", "ts", "js", "go"].contains(&ext) {
            continue;
        }

        if let Ok(content) = std::fs::read_to_string(path) {
            let routes = scan_file_for_routes(path, &content);
            for route in routes {
                let node = GraphNode::new(
                    NodeType::HttpRoute,
                    format!("{}:{}", route.method, route.path),
                    route.handler_path.clone(),
                );
                graph.upsert_node(node)?;
                count += 1;
            }
        }
    }

    Ok(count)
}