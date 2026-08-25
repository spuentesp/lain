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
    /// `None` for APIs that carry no verb at the call site (Go's
    /// `http.HandleFunc`), where every route defaults to GET. Modelled
    /// as an absent regex rather than one written so it can never match,
    /// which reads as a bug to anyone who finds it later.
    method_regex: Option<regex::Regex>,
    path_regex: regex::Regex,
    handler_fn_regex: regex::Regex,
}

impl RoutePattern {
    fn new(method_pat: &str, path_pat: &str, handler_pat: &str) -> Self {
        Self {
            method_regex: Some(regex::Regex::new(method_pat).unwrap()),
            path_regex: regex::Regex::new(path_pat).unwrap(),
            handler_fn_regex: regex::Regex::new(handler_pat).unwrap(),
        }
    }

    /// For route APIs with no verb at the call site; routes default to GET.
    fn without_method(path_pat: &str, handler_pat: &str) -> Self {
        Self {
            method_regex: None,
            path_regex: regex::Regex::new(path_pat).unwrap(),
            handler_fn_regex: regex::Regex::new(handler_pat).unwrap(),
        }
    }

    /// Extract routes from `content`.
    ///
    /// The route declaration and its handler are matched over a small
    /// forward window, not a single line. This used to require method,
    /// path *and* handler to all appear on one line, which is true of
    /// Gin (`r.GET("/x", h)`) and Express and essentially nothing else:
    /// the two shapes people actually write —
    ///
    /// ```text
    /// #[get("/api/users")]              @app.get("/api/users")
    /// async fn list_users() -> ...      async def list_users():
    /// ```
    ///
    /// put the handler on the *next* line, so every Actix and FastAPI
    /// route in existence was silently skipped. Nothing noticed because
    /// the sensors had no caller at all.
    fn extract(&self, content: &str, file_path: &str) -> Vec<HttpRoute> {
        const HANDLER_LOOKAHEAD: usize = 6;

        let lines: Vec<&str> = content.lines().collect();
        let mut routes = Vec::new();

        for (idx, line) in lines.iter().enumerate() {
            // A route declaration is identified by its path. Without one
            // there is nothing to attach a handler to.
            let Some(path) = self
                .path_regex
                .captures(line)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
            else {
                continue;
            };
            if path.is_empty() {
                continue;
            }

            let method = self
                .method_regex
                .as_ref()
                .and_then(|re| re.captures(line))
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_uppercase())
                .unwrap_or_else(|| "GET".to_string());

            // Prefer a handler on the declaring line (Gin, Express);
            // otherwise look ahead for the function it decorates
            // (Actix, FastAPI, Flask). Stop at the first hit so a
            // decorator cannot claim a function further down the file
            // than its own.
            let mut handler = self
                .handler_fn_regex
                .captures(line)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string());

            if handler.is_none() {
                for look in lines.iter().skip(idx + 1).take(HANDLER_LOOKAHEAD) {
                    if let Some(h) = self
                        .handler_fn_regex
                        .captures(look)
                        .and_then(|c| c.get(1))
                        .map(|m| m.as_str().to_string())
                    {
                        handler = Some(h);
                        break;
                    }
                }
            }

            let Some(handler) = handler else { continue };
            if handler.is_empty() {
                continue;
            }

            routes.push(HttpRoute {
                method,
                path,
                handler_path: file_path.to_string(),
                handler_name: handler,
                line: idx as u32 + 1,
            });
        }
        routes
    }
}

/// All supported route patterns.
///
/// Several of these did not match the syntax they were named for.
/// `rust-axum` keyed off a `route!` macro that axum does not have;
/// `rust-actix` only accepted `#[get(path = "...")]` and not the
/// ordinary `#[get("...")]`; the two TypeScript patterns hard-coded
/// `.get` in their path and handler regexes, so a POST or DELETE route
/// produced no path at all; and `go-gin` required the router variable to
/// be named literally `r`. None of it was noticed because
/// `server::sensors` had no caller.
fn get_route_patterns() -> HashMap<&'static str, RoutePattern> {
    let mut patterns = HashMap::new();

    const HTTP_VERBS: &str = "get|post|put|delete|patch|options|head";

    // Rust: axum — `.route("/path", get(handler))`
    patterns.insert("rust-axum", RoutePattern::new(
        &format!(r"\.route\s*\([^,]*,\s*(?i:({HTTP_VERBS}))\s*\("),
        r#"\.route\s*\(\s*"([^"]+)""#,
        &format!(r"(?i:(?:{HTTP_VERBS}))\s*\(\s*(\w+)\s*[,)]"),
    ));

    // Rust: actix-web — `#[get("/path")]` or `#[get(path = "/path")]`,
    // handler on the following line.
    patterns.insert("rust-actix", RoutePattern::new(
        &format!(r"#\[(?i:({HTTP_VERBS}))\s*\("),
        &format!(r#"#\[(?i:(?:{HTTP_VERBS}))\s*\(\s*(?:path\s*=\s*)?"([^"]+)""#),
        r"(?:async\s+)?fn\s+(\w+)\s*[(<]",
    ));

    // Python: FastAPI — `@app.get("/path")` then `async def handler(...)`
    patterns.insert("python-fastapi", RoutePattern::new(
        &format!(r"@[\w\.]+\.({HTTP_VERBS})\s*\("),
        &format!(r#"@[\w\.]+\.(?:{HTTP_VERBS})\s*\(\s*["']([^"']+)["']"#),
        r"(?:async\s+)?def\s+(\w+)\s*\(",
    ));

    // Python: Flask — `@app.route("/path", methods=["POST"])` then `def handler(...)`
    patterns.insert("python-flask", RoutePattern::new(
        r#"methods\s*=\s*\[\s*["'](\w+)"#,
        r#"@[\w\.]+\.route\s*\(\s*["']([^"']+)["']"#,
        r"def\s+(\w+)\s*\(",
    ));

    // TypeScript/JS: Express / Fastify — `router.post("/path", handler)`
    patterns.insert("ts-express", RoutePattern::new(
        &format!(r"\.({HTTP_VERBS})\s*\("),
        &format!(r#"\.(?:{HTTP_VERBS})\s*\(\s*["'`]([^"'`]+)["'`]"#),
        &format!(r#"\.(?:{HTTP_VERBS})\s*\(\s*["'`][^"'`]+["'`]\s*,\s*(?:async\s*)?(\w+)"#),
    ));

    // Go: net/http — `http.HandleFunc("/path", handler)`; the API
    // carries no verb, so the default GET applies.
    patterns.insert("go-std", RoutePattern::without_method(
        r#"HandleFunc\s*\(\s*"([^"]+)""#,
        r#"HandleFunc\s*\(\s*"[^"]+"\s*,\s*(\w+)"#,
    ));

    // Go: Gin / Echo — `router.GET("/path", handler)`, any receiver name.
    patterns.insert("go-gin", RoutePattern::new(
        r"\.(GET|POST|PUT|DELETE|PATCH|OPTIONS|HEAD)\s*\(",
        r#"\.(?:GET|POST|PUT|DELETE|PATCH|OPTIONS|HEAD)\s*\(\s*"([^"]+)""#,
        r#"\.(?:GET|POST|PUT|DELETE|PATCH|OPTIONS|HEAD)\s*\(\s*"[^"]+"\s*,\s*(\w+)"#,
    ));

    patterns
}

/// Scan a file for HTTP routes
pub fn scan_file_for_routes(path: &std::path::Path, content: &str) -> Vec<HttpRoute> {
    let extension = path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    // `extension` was computed and then dropped: every pattern set ran
    // against every file, so Go's `r.GET("/x", h)` pattern was matched
    // against Python sources and vice versa. Nothing caught it because
    // this file was never compiled. Scope the patterns to the language
    // the extension actually names.
    let all_patterns = get_route_patterns();
    let prefixes: &[&str] = match extension {
        "rs" => &["rust-"],
        "py" => &["python-"],
        "ts" | "tsx" | "js" | "jsx" => &["ts-"],
        "go" => &["go-"],
        _ => return Vec::new(),
    };
    let applicable: Vec<&RoutePattern> = all_patterns
        .iter()
        .filter(|(k, _)| prefixes.iter().any(|p| k.starts_with(p)))
        .map(|(_, v)| v)
        .collect();

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
        let node_id = GraphNode::generate_id(&NodeType::HttpRoute, &route.handler_path, &format!("{}:{}", route.method, route.path), None);

        let mut node = GraphNode::new(
            NodeType::HttpRoute,
            format!("{} {}", route.method, route.path),
            route.handler_path.clone(),
        );
        node.id = node_id.clone();
        node.line_start = Some(route.line);
        node.signature = Some(route.handler_name.clone());

        nodes.push(node);

        // `find_nodes_by_name` never existed on `GraphDatabase`; this file
        // was not declared in `sensors/mod.rs`, so nothing ever type-checked
        // the call. Resolve the way the rest of the indexer does: prefer a
        // handler defined in the same file as the route, and emit nothing
        // when the name is ambiguous across files. A missing edge is a gap,
        // N wrong edges are a lie — and a wrong `CallsHttp` edge would
        // attribute an endpoint to a handler that does not serve it.
        let candidates = graph.find_all_nodes_by_name(&route.handler_name);
        let handler = match candidates.len() {
            0 => None,
            1 => candidates.into_iter().next(),
            _ => candidates
                .into_iter()
                .find(|n| n.path == route.handler_path),
        };
        if let Some(handler) = handler {
            edges.push(GraphEdge::new(EdgeType::CallsHttp, node_id, handler.id));
        }
    }

    (nodes, edges)
}

/// Scan a directory tree for HTTP routes and add to graph
pub fn scan_workspace_routes(
    graph: &GraphDatabase,
    root: &std::path::Path,
) -> Result<usize, LainError> {
    if graph.is_read_only() {
        return Ok(0);
    }
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
            let mut routes = scan_file_for_routes(path, &content);
            if routes.is_empty() {
                continue;
            }
            // Mint node paths with the same helper the scanner uses.
            // The walker hands back absolute paths, while every other
            // node in the graph is keyed relative to the workspace, so
            // an absolute `handler_path` made the route node look
            // untracked: the orphan sweep compares against
            // `graph_path`-reduced tracked files and pruned every route
            // it had just created. The sensor ran, reported a node, and
            // the node was gone by the end of the same index pass.
            for r in &mut routes {
                r.handler_path = crate::graph::graph_path(root, std::path::Path::new(&r.handler_path));
            }
            // Go through `routes_to_graph` rather than building nodes
            // inline. The inline version emitted *no* `CallsHttp` edges at
            // all, so `get_cross_runtime_callers` — whose whole job is to
            // filter on that edge — would answer nothing even once this
            // scan was wired into ingestion. It also disagreed with
            // `routes_to_graph` on both the node name (`"GET:/x"` vs
            // `"GET /x"`) and the id, so the two paths produced different
            // nodes for the same route.
            let (nodes, edges) = routes_to_graph(graph, &routes);
            for node in nodes {
                graph.upsert_node(node)?;
                count += 1;
            }
            // Emitted after the nodes exist so the edge endpoints resolve.
            graph.insert_edges_batch(&edges)?;
        }
    }

    Ok(count)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::NodeType;

    fn temp_graph(tag: &str) -> GraphDatabase {
        let tmp = std::env::temp_dir().join(format!("http_sensor_{tag}"));
        let _ = std::fs::remove_dir_all(&tmp);
        GraphDatabase::new(&tmp).unwrap()
    }

    /// This whole module was undeclared in `sensors/mod.rs`, so it never
    /// compiled — the two Python patterns carried malformed raw-string
    /// literals (`r"..."#`) that would have been hard syntax errors in any
    /// build. Constructing the patterns at all is the regression test.
    #[test]
    fn every_route_pattern_compiles() {
        let patterns = get_route_patterns();
        assert!(
            patterns.contains_key("python-fastapi") && patterns.contains_key("python-flask"),
            "the two patterns whose raw strings were malformed must be present"
        );
    }

    /// The shapes people actually write put the handler on the line
    /// *after* the route declaration. The old line-by-line extractor
    /// required method, path and handler to coincide on one line, so
    /// every Actix, FastAPI and Flask route was silently skipped.
    #[test]
    fn a_handler_on_the_following_line_is_found() {
        let actix = "#[get(\"/api/users\")]\nasync fn list_users() -> impl Responder {\n    todo!()\n}\n";
        let r = scan_file_for_routes(std::path::Path::new("api.rs"), actix);
        assert_eq!(r.len(), 1, "actix route should be found: {r:?}");
        assert_eq!(r[0].method, "GET");
        assert_eq!(r[0].path, "/api/users");
        assert_eq!(r[0].handler_name, "list_users");

        let fastapi = "@app.post(\"/api/widgets\")\nasync def create_widget(body: Widget):\n    ...\n";
        let r = scan_file_for_routes(std::path::Path::new("api.py"), fastapi);
        assert_eq!(r.len(), 1, "fastapi route should be found: {r:?}");
        assert_eq!(r[0].method, "POST");
        assert_eq!(r[0].path, "/api/widgets");
        assert_eq!(r[0].handler_name, "create_widget");
    }

    /// Flask puts the verb in a `methods=` kwarg and the handler below.
    #[test]
    fn flask_routes_pick_up_their_method_and_handler() {
        let flask = "@app.route(\"/api/orders\", methods=[\"POST\"])\ndef create_order():\n    pass\n";
        let r = scan_file_for_routes(std::path::Path::new("app.py"), flask);
        assert_eq!(r.len(), 1, "flask route should be found: {r:?}");
        assert_eq!(r[0].method, "POST");
        assert_eq!(r[0].path, "/api/orders");
        assert_eq!(r[0].handler_name, "create_order");
    }

    /// `rust-axum` keyed off a `route!` macro axum does not have, so no
    /// axum route ever matched.
    #[test]
    fn axum_route_calls_are_recognised() {
        let axum = "let app = Router::new()\n    .route(\"/api/health\", get(health_check));\n";
        let r = scan_file_for_routes(std::path::Path::new("main.rs"), axum);
        assert_eq!(r.len(), 1, "axum route should be found: {r:?}");
        assert_eq!(r[0].method, "GET");
        assert_eq!(r[0].path, "/api/health");
        assert_eq!(r[0].handler_name, "health_check");
    }

    /// Express's path and handler regexes were hard-coded to `.get`, so
    /// a POST route produced no path and was dropped.
    #[test]
    fn express_non_get_verbs_are_not_dropped() {
        let ts = "router.post(\"/api/login\", loginHandler);\n";
        let r = scan_file_for_routes(std::path::Path::new("routes.ts"), ts);
        assert_eq!(r.len(), 1, "express POST route should be found: {r:?}");
        assert_eq!(r[0].method, "POST");
        assert_eq!(r[0].path, "/api/login");
        assert_eq!(r[0].handler_name, "loginHandler");
    }

    /// Gin required the router variable to be named literally `r`.
    #[test]
    fn gin_routes_work_with_any_receiver_name() {
        for src in [
            "r.GET(\"/api/users\", listUsers)\n",
            "router.GET(\"/api/users\", listUsers)\n",
            "engine.GET(\"/api/users\", listUsers)\n",
        ] {
            let r = scan_file_for_routes(std::path::Path::new("routes.go"), src);
            assert_eq!(r.len(), 1, "gin route should be found in {src:?}: {r:?}");
            assert_eq!(r[0].handler_name, "listUsers");
        }
    }

    /// A decorator must not adopt a function far below it, or an
    /// unrelated symbol becomes the "handler" for a route.
    #[test]
    fn a_declaration_does_not_claim_a_distant_function() {
        let mut src = String::from("#[get(\"/api/users\")]\n");
        for _ in 0..12 {
            src.push_str("// filler\n");
        }
        src.push_str("async fn much_later() {}\n");
        let r = scan_file_for_routes(std::path::Path::new("api.rs"), &src);
        assert!(
            r.is_empty(),
            "a handler 13 lines away must not be attached: {r:?}"
        );
    }

    /// Go's `HandleFunc` carries no verb, so its routes default to GET.
    #[test]
    fn a_verbless_route_api_defaults_to_get() {
        let go = "http.HandleFunc(\"/healthz\", healthz)\n";
        let r = scan_file_for_routes(std::path::Path::new("main.go"), go);
        assert_eq!(r.len(), 1, "net/http route should be found: {r:?}");
        assert_eq!(r[0].method, "GET");
        assert_eq!(r[0].path, "/healthz");
        assert_eq!(r[0].handler_name, "healthz");
    }

    /// `scan_file_for_routes` computed `extension` and then ignored it, so
    /// Go's route regexes ran against Python files and vice versa.
    #[test]
    fn patterns_are_scoped_to_the_files_language() {
        let go_source = r#"r.GET("/api/users", listUsers)"#;

        let as_go = scan_file_for_routes(std::path::Path::new("routes.go"), go_source);
        assert_eq!(as_go.len(), 1, "gin route should be found in a .go file");
        assert_eq!(as_go[0].method, "GET");
        assert_eq!(as_go[0].path, "/api/users");
        assert_eq!(as_go[0].handler_name, "listUsers");

        let as_python = scan_file_for_routes(std::path::Path::new("routes.py"), go_source);
        assert!(
            as_python.is_empty(),
            "Go route syntax must not be matched by the Python patterns: {as_python:?}"
        );

        let unknown = scan_file_for_routes(std::path::Path::new("notes.txt"), go_source);
        assert!(unknown.is_empty(), "an unhandled extension yields no routes");
    }

    /// `get_cross_runtime_callers` filters on `CallsHttp`, so a route with
    /// no edge to its handler is invisible to the only tool that reads it.
    #[test]
    fn a_route_gets_a_callshttp_edge_to_its_handler() {
        let graph = temp_graph("edge");
        let handler = GraphNode::new(
            NodeType::Function,
            "listUsers".to_string(),
            "routes.go".to_string(),
        );
        graph.upsert_node(handler.clone()).unwrap();

        let routes = scan_file_for_routes(
            std::path::Path::new("routes.go"),
            r#"r.GET("/api/users", listUsers)"#,
        );
        let (nodes, edges) = routes_to_graph(&graph, &routes);

        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].node_type, NodeType::HttpRoute);
        assert_eq!(
            edges.len(),
            1,
            "the route must be linked to its handler with CallsHttp"
        );
        assert_eq!(edges[0].edge_type, EdgeType::CallsHttp);
        assert_eq!(edges[0].target_id, handler.id);
    }

    /// The workspace scan built nodes inline instead of going through
    /// `routes_to_graph`, so it emitted zero `CallsHttp` edges and named
    /// nodes differently than the other path. Wiring it up would have
    /// produced routes that no tool could traverse.
    #[test]
    fn the_workspace_scan_emits_edges_not_just_nodes() {
        let dir = std::env::temp_dir().join("http_sensor_scan_ws");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("routes.go"), "r.GET(\"/api/users\", listUsers)\n").unwrap();

        let graph = temp_graph("scan");
        graph
            .upsert_node(GraphNode::new(
                NodeType::Function,
                "listUsers".to_string(),
                dir.join("routes.go").to_string_lossy().to_string(),
            ))
            .unwrap();

        let count = scan_workspace_routes(&graph, &dir).unwrap();
        assert_eq!(count, 1, "one route node created");

        let http_nodes: Vec<_> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::HttpRoute)
            .collect();
        assert_eq!(http_nodes.len(), 1);
        assert_eq!(
            http_nodes[0].name, "GET /api/users",
            "node naming must match routes_to_graph, not the old inline `GET:/path` form"
        );

        let callshttp = graph
            .all_edges()
            .into_iter()
            .filter(|e| e.edge_type == EdgeType::CallsHttp)
            .count();
        assert_eq!(callshttp, 1, "the scan must persist the CallsHttp edge");
    }

    /// An ambiguous handler name resolves to the route's own file rather
    /// than guessing — the same rule the tree-sitter resolver follows.
    #[test]
    fn an_ambiguous_handler_resolves_to_the_routes_own_file() {
        let graph = temp_graph("ambig");
        let same_file = GraphNode::new(
            NodeType::Function,
            "listUsers".to_string(),
            "routes.go".to_string(),
        );
        let other_file = GraphNode::new(
            NodeType::Function,
            "listUsers".to_string(),
            "elsewhere.go".to_string(),
        );
        graph.upsert_node(same_file.clone()).unwrap();
        graph.upsert_node(other_file).unwrap();

        let routes = scan_file_for_routes(
            std::path::Path::new("routes.go"),
            r#"r.GET("/api/users", listUsers)"#,
        );
        let (_, edges) = routes_to_graph(&graph, &routes);

        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].target_id, same_file.id,
            "must pick the handler defined alongside the route"
        );
    }
}
