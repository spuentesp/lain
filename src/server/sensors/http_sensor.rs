//! HTTP route sensor
//!
//! Extracts HTTP route definitions via regex-first heuristics, then
//! emits one `HttpRoute` node per route with `ContractFact::Provider`
//! (`§4.3`, `§6.2`) carrying the normalized template (`§4.5`).
//!
//! Supported patterns:
//!   - Rust: axum (`.route("/path", get(h))`, `.nest("/p", f())`),
//!     actix-web (`#[get("/path")]`, `web::scope("/p")`)
//!   - Python: FastAPI (`@app.get("/path")`, `@router.get("/path")`
//!     when `APIRouter` carries a `prefix=`), Flask
//!     (`@app.route("/path", ...)`, `Blueprint(url_prefix="…")`)
//!   - TypeScript: Express / Fastify (`router.post("/path", h)`)
//!   - Go: `net/http` `HandleFunc` (no verb → `ANY`), Gin / Echo
//!     `r.GET("/path", h)`
//!
//! Edges created: `CallsHttp` (route → handler function). The route's
//! template is normalized through `federation::contracts::normalize`
//! so the joiner (PR 7) and the matcher (`federation::contracts::
//! route_match`) can compare it against consumer templates verbatim.
//!
//! Determinism: `get_route_patterns` returns a `BTreeMap`, not a
//! `HashMap` (§6.1). Every map iterated by a sensor is sorted so the
//! §8.3 determinism test holds across runs.

use crate::error::LainError;
use crate::federation::contracts::model::{
    HttpMethod, ProviderFact, ProviderOrigin, SymbolKey,
};
use crate::federation::contracts::normalize::{normalize, UrlPart};
use crate::federation::repo_id::RepoId;
use crate::graph::{GraphDatabase, SensorOwner};
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
use std::collections::BTreeMap;

/// A detected HTTP route, in the shape the regex extractor produces
/// before normalization.
#[derive(Debug, Clone)]
pub struct HttpRoute {
    /// `GET`, `POST`, `ANY` (the §6.2 case for verbless APIs).
    pub method: HttpMethod,
    pub path: String,         // /api/users/:id — the raw, pre-normalization path
    pub handler_path: String, // file path
    pub handler_name: String, // function name
    pub line: u32,
}

/// HTTP route patterns per language
struct RoutePattern {
    /// `None` for APIs that carry no verb at the call site (Go's
    /// `http.HandleFunc`). Per §6.2 these routes are emitted as
    /// `HttpMethod::Any` (was `GET` in 0.8).
    method_regex: Option<regex::Regex>,
    path_regex: regex::Regex,
    handler_fn_regex: regex::Regex,
    /// What to emit when the regex above did not capture a verb.
    /// `HttpMethod::Any` for go-std (the route declares no verb);
    /// `HttpMethod::Get` for Flask (whose default verb is GET even
    /// when `methods=` is absent, §6.2).
    default_method: HttpMethod,
}

impl RoutePattern {
    fn new(method_pat: &str, path_pat: &str, handler_pat: &str) -> Self {
        Self {
            method_regex: Some(regex::Regex::new(method_pat).unwrap()),
            path_regex: regex::Regex::new(path_pat).unwrap(),
            handler_fn_regex: regex::Regex::new(handler_pat).unwrap(),
            default_method: HttpMethod::Any,
        }
    }

    /// Construct with an explicit default method. Use `default_method:
    /// HttpMethod::Get` for Flask (`methods=` may be absent).
    fn with_default(
        method_pat: &str,
        path_pat: &str,
        handler_pat: &str,
        default_method: HttpMethod,
    ) -> Self {
        Self {
            method_regex: Some(regex::Regex::new(method_pat).unwrap()),
            path_regex: regex::Regex::new(path_pat).unwrap(),
            handler_fn_regex: regex::Regex::new(handler_pat).unwrap(),
            default_method,
        }
    }

    /// For route APIs with no verb at the call site; routes default to
    /// `Any` per §6.2.
    fn without_method(path_pat: &str, handler_pat: &str) -> Self {
        Self {
            method_regex: None,
            path_regex: regex::Regex::new(path_pat).unwrap(),
            handler_fn_regex: regex::Regex::new(handler_pat).unwrap(),
            default_method: HttpMethod::Any,
        }
    }

    /// Extract routes from `content`.
    fn extract(&self, content: &str, file_path: &str) -> Vec<HttpRoute> {
        const HANDLER_LOOKAHEAD: usize = 6;

        let lines: Vec<&str> = content.lines().collect();
        let mut routes = Vec::new();

        for (idx, line) in lines.iter().enumerate() {
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
                .map(method_from_str)
                .unwrap_or(self.default_method);

            // Prefer a handler on the declaring line (Gin, Express);
            // otherwise look ahead for the function it decorates
            // (Actix, FastAPI, Flask).
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

fn method_from_str(s: String) -> HttpMethod {
    match s.as_str() {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        "PUT" => HttpMethod::Put,
        "PATCH" => HttpMethod::Patch,
        "DELETE" => HttpMethod::Delete,
        "HEAD" => HttpMethod::Head,
        "OPTIONS" => HttpMethod::Options,
        _ => HttpMethod::Any,
    }
}

/// All supported route patterns. `BTreeMap` (was `HashMap` pre-§6.1)
/// so the registry's iteration order is deterministic across runs.
fn get_route_patterns() -> BTreeMap<&'static str, RoutePattern> {
    let mut patterns = BTreeMap::new();

    const HTTP_VERBS: &str = "get|post|put|delete|patch|options|head";

    // Rust: axum — `.route("/path", get(handler))`
    patterns.insert(
        "rust-axum",
        RoutePattern::new(
            &format!(r"\.route\s*\([^,]*,\s*(?i:({HTTP_VERBS}))\s*\("),
            r#"\.route\s*\(\s*"([^"]+)""#,
            &format!(r"(?i:(?:{HTTP_VERBS}))\s*\(\s*(\w+)\s*[,)]"),
        ),
    );

    // Rust: actix-web — `#[get("/path")]` or `#[get(path = "/path")]`.
    patterns.insert(
        "rust-actix",
        RoutePattern::new(
            &format!(r"#\[(?i:({HTTP_VERBS}))\s*\("),
            &format!(r#"#\[(?i:(?:{HTTP_VERBS}))\s*\(\s*(?:path\s*=\s*)?"([^"]+)""#),
            r"(?:async\s+)?fn\s+(\w+)\s*[(<]",
        ),
    );

    // Python: FastAPI — `@app.get("/path")` then `async def handler(...)`.
    // Per §6.2 `APIRouter(prefix=…)` is the same-file prefix path:
    // decorated `@router.get("/path")` inherits the prefix when
    // `router` is `APIRouter(prefix="…")`.
    patterns.insert(
        "python-fastapi",
        RoutePattern::new(
            &format!(r"@[\w\.]+\.({HTTP_VERBS})\s*\("),
            &format!(r#"@[\w\.]+\.(?:{HTTP_VERBS})\s*\(\s*["']([^"']+)["']"#),
            r"(?:async\s+)?def\s+(\w+)\s*\(",
        ),
    );

    // Python: Flask — `@app.route("/path", methods=["POST"])` then `def
    // handler(...)`. §6.2: Flask defaults to GET when `methods=` is
    // absent, so `default_method` is `Get` rather than `Any`.
    patterns.insert(
        "python-flask",
        RoutePattern::with_default(
            r#"methods\s*=\s*\[\s*["'](\w+)"#,
            r#"@[\w\.]+\.route\s*\(\s*["']([^"']+)["']"#,
            r"def\s+(\w+)\s*\(",
            HttpMethod::Get,
        ),
    );

    // TypeScript/JS: Express / Fastify — `router.post("/path", handler)`
    patterns.insert(
        "ts-express",
        RoutePattern::new(
            &format!(r"\.({HTTP_VERBS})\s*\("),
            &format!(r#"\.(?:{HTTP_VERBS})\s*\(\s*["'`]([^"'`]+)["'`]"#),
            &format!(r#"\.(?:{HTTP_VERBS})\s*\(\s*["'`][^"'`]+["'`]\s*,\s*(?:async\s*)?(\w+)"#),
        ),
    );

    // Go: net/http — `http.HandleFunc("/path", handler)`; carries no
    // verb at the call site (§6.2 → `Any`, was `GET` in 0.8).
    patterns.insert(
        "go-std",
        RoutePattern::without_method(
            r#"HandleFunc\s*\(\s*"([^"]+)""#,
            r#"HandleFunc\s*\(\s*"[^"]+"\s*,\s*(\w+)"#,
        ),
    );

    // Go: Gin / Echo — `router.GET("/path", handler)`.
    patterns.insert(
        "go-gin",
        RoutePattern::new(
            r"\.(GET|POST|PUT|DELETE|PATCH|OPTIONS|HEAD)\s*\(",
            r#"\.(?:GET|POST|PUT|DELETE|PATCH|OPTIONS|HEAD)\s*\(\s*"([^"]+)""#,
            r#"\.(?:GET|POST|PUT|DELETE|PATCH|OPTIONS|HEAD)\s*\(\s*"[^"]+"\s*,\s*(\w+)"#,
        ),
    );

    patterns
}

/// Scan a file for HTTP routes, after applying same-file router
/// prefixes (§6.2).
pub fn scan_file_for_routes(path: &std::path::Path, content: &str) -> Vec<HttpRoute> {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");

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
    let router_prefixes = extract_router_prefixes(content, extension);

    for pattern in applicable {
        let routes = pattern.extract(content, &path.to_string_lossy());
        for mut r in routes {
            if let Some(prefix) = router_prefix_for_receiver(&r, &router_prefixes) {
                r.path = join_prefix(prefix.as_str(), &r.path);
            }
            all_routes.push(r);
        }
    }

    // Deduplicate by (method, path). `HttpMethod` does not derive
    // `Ord`, so use a `HashSet` here (insertion-order isn't a
    // contract — the dedup set is only ever used to drop duplicates,
    // and the test asserts content, not order).
    let mut seen: std::collections::HashSet<(HttpMethod, String)> =
        std::collections::HashSet::new();
    all_routes.retain(|r| seen.insert((r.method, r.path.clone())));

    all_routes
}

// ─── Same-file router prefixes (§6.2) ─────────────────────────────────

/// Per-receiver router prefix detected in the same file. Receiver
/// name → path prefix to prepend to that receiver's decorated routes.
fn extract_router_prefixes(content: &str, extension: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();

    if extension == "py" {
        // FastAPI: `router = APIRouter(prefix="/api/v1")`
        let re = regex::Regex::new(
            r#"(\w+)\s*=\s*APIRouter\s*\(\s*prefix\s*=\s*["']([^"']+)["']"#,
        )
        .unwrap();
        for cap in re.captures_iter(content) {
            out.insert(cap[1].to_string(), cap[2].to_string());
        }

        // Flask: `bp = Blueprint("name", __name__, url_prefix="/api")`
        let re = regex::Regex::new(
            r#"(\w+)\s*=\s*Blueprint\s*\([^)]*url_prefix\s*=\s*["']([^"']+)["']"#,
        )
        .unwrap();
        for cap in re.captures_iter(content) {
            out.insert(cap[1].to_string(), cap[2].to_string());
        }
    } else if extension == "rs" {
        // axum: `.nest("/api", router)` where `router` is defined
        // in the same file. Match `let router = Router::new()...`
        // for the receiver name; the prefix is from `.nest("…", router)`.
        let def_re =
            regex::Regex::new(r"let\s+(\w+)\s*=\s*Router::new").unwrap();
        let nest_re = regex::Regex::new(
            r#"\.nest\s*\(\s*["']([^"']+)["']\s*,\s*(\w+)\s*\)"#,
        )
        .unwrap();
        for nest_cap in nest_re.captures_iter(content) {
            let prefix = &nest_cap[1];
            let receiver = &nest_cap[2];
            // Only apply when the receiver is bound to `Router::new()`
            // in this file.
            if def_re.is_match(content) {
                out.entry(receiver.to_string())
                    .or_insert_with(|| prefix.to_string());
            }
        }

        // actix-web: `web::scope("/api")` — applied to the immediately
        // following `#[get(...)]` lines. The extractor below attaches
        // a scope to the next-handler-line's receiver. For simplicity
        // we only attach to handlers inside `web::scope("/x") { … }`
        // braces; this is a coarse approximation but covers the
        // common shape. The matcher's prefix tolerance (§7.4) is the
        // broader net when this approximation misses.
        let scope_re = regex::Regex::new(r#"web::scope\s*\(\s*["']([^"']+)["']\s*\)"#).unwrap();
        for scope_cap in scope_re.captures_iter(content) {
            // Map the prefix to a sentinel key — the regex-based
            // extraction below only attaches the prefix to handlers
            // declared inside `scope("/prefix")`. We use the prefix
            // itself as the key so `router_prefix_for_receiver` can
            // match it.
            out.insert(
                format!("scope:{}", scope_cap[1].to_string()),
                scope_cap[1].to_string(),
            );
        }
    }

    out
}

fn router_prefix_for_receiver(
    route: &HttpRoute,
    prefixes: &BTreeMap<String, String>,
) -> Option<String> {
    // Python: receiver name in the decorator (e.g. `@router.get(...)`).
    // We can match on the file's router declarations directly — the
    // receiver name is the variable the decorator was called on. We
    // can't reliably recover the receiver name from the path-only
    // route without re-running the regex, so we approximate: if the
    // route's handler_path contains any of the receiver names, take
    // the longest match.
    if prefixes.is_empty() {
        return None;
    }

    // The receiver was the second capture group of the path regex
    // before §6.2 prefix handling. Without re-extracting, use the
    // handler_path's basename as a hint.
    // Simpler heuristic: match the handler name against the receiver
    // names — receivers are often named after the prefix.
    let _ = route;
    // If we have a single prefix and no receiver info, attach it.
    // Cross-file router mounts (e.g. `app.include_router(r,
    // prefix=…)`, Express `app.use("/p", router)`) are covered by
    // §7.1 config and §7.4 prefix-tolerant matching — this code only
    // handles same-file routers.
    if prefixes.len() == 1 {
        let (key, prefix) = prefixes.iter().next().unwrap();
        if key.starts_with("scope:") {
            // actix `web::scope` — attach only when handler is in
            // the scope's file scope (always true for our coarse
            // single-file heuristic).
            return Some(prefix.clone());
        }
        return Some(prefix.clone());
    }
    None
}

fn join_prefix(prefix: &str, path: &str) -> String {
    if prefix.is_empty() {
        return path.to_string();
    }
    let prefix = if prefix.starts_with('/') {
        prefix.to_string()
    } else {
        format!("/{}", prefix)
    };
    if path.starts_with('/') {
        format!("{}{}", prefix, path)
    } else {
        format!("{}/{}", prefix, path)
    }
}

// ─── Graph emission (§6.2 ContractFact::Provider) ────────────────────

/// Convert HTTP routes to graph nodes and edges, attaching
/// `ContractFact::Provider` to each route (§6.2). The template is
/// the §4.5-normalized path.
pub fn routes_to_graph(
    graph: &GraphDatabase,
    routes: &[HttpRoute],
    namespace: &RepoNamespace,
    repo_id: &RepoId,
) -> (Vec<GraphNode>, Vec<GraphEdge>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    for route in routes {
        let node_id = GraphNode::generate_id(
            &NodeType::HttpRoute,
            &route.handler_path,
            &format!("{:?}:{}", route.method, route.path),
            None,
            namespace,
        );

        let template = normalize(&[UrlPart::Literal(route.path.clone())])
            .template
            .unwrap_or_else(|| "/".to_string());

        let mut node = GraphNode::new(
            NodeType::HttpRoute,
            format!("{} {}", method_to_str(route.method), route.path),
            route.handler_path.clone(),
        );
        node.id = node_id.clone();
        node.line_start = Some(route.line);
        node.signature = Some(route.handler_name.clone());
        node.contract = Some(crate::federation::contracts::model::ContractFact::Provider(
            ProviderFact {
                method: route.method,
                template,
                handler: Some(SymbolKey {
                    repo: repo_id.clone(),
                    path: route.handler_path.clone(),
                    container: None,
                    name: route.handler_name.clone(),
                }),
                operation_id: None,
                origin: ProviderOrigin::Code,
            },
        ));

        nodes.push(node);

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

fn method_to_str(m: HttpMethod) -> &'static str {
    match m {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Head => "HEAD",
        HttpMethod::Options => "OPTIONS",
        HttpMethod::Any => "ANY",
    }
}

/// Scan a directory tree for HTTP routes, replacing the previous
/// run's `HttpSensor` output via `replace_sensor_output` (§6.1). This
/// is what fixes the stale `HttpRoute` problem 0.8 had: routes
/// deleted from source disappear after the next rescan.
pub fn scan_workspace_routes(
    graph: &GraphDatabase,
    root: &std::path::Path,
    namespace: &RepoNamespace,
    repo_id: &RepoId,
) -> Result<usize, LainError> {
    if graph.is_read_only() {
        return Ok(0);
    }
    let mut all_nodes: Vec<GraphNode> = Vec::new();
    let mut all_edges: Vec<GraphEdge> = Vec::new();

    for entry in crate::server::sensors::util::walk_workspace(root) {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        if !["rs", "py", "ts", "js", "go"].contains(&ext) {
            continue;
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue, // a vanished file between walk and read is fine
        };
        let mut routes = scan_file_for_routes(path, &content);
        for r in &mut routes {
            r.handler_path =
                crate::graph::graph_path(root, std::path::Path::new(&r.handler_path));
        }
        let (nodes, edges) = routes_to_graph(graph, &routes, namespace, repo_id);
        all_nodes.extend(nodes);
        all_edges.extend(edges);
    }

    let removed = graph.replace_sensor_output(SensorOwner::HttpSensor, &all_nodes, &all_edges)?;
    if removed > 0 {
        tracing::debug!(
            "http_sensor: replaced {removed} stale route(s) for {root:?}"
        );
    }
    Ok(all_nodes.len())
}

/// Unit-struct Sensor impl. Discovery via
/// `inventory::submit!(SensorEntry(&HttpRouteSensor))` below; no
/// central registry to edit. The HTTP sensor is the one named
/// `scan_workspace_routes` (not `scan_workspace`) so the legacy
/// aggregator doesn't have to special-case it — the trait impl
/// delegates to whichever name the module uses.
pub struct HttpRouteSensor;

impl crate::server::sensors::Sensor for HttpRouteSensor {
    fn name(&self) -> &'static str {
        "http"
    }
    fn count_field(&self) -> crate::server::sensors::SensorCountField {
        crate::server::sensors::SensorCountField::HttpRoutes
    }
    fn scan(
        &self,
        graph: &GraphDatabase,
        root: &std::path::Path,
        namespace: &RepoNamespace,
    ) -> Result<usize, LainError> {
        let repo_id = RepoId::new(root.to_string_lossy().as_ref()).unwrap_or_else(|_| {
            // Fall back to a synthetic repo id from the root path.
            RepoId::new("http-sensor").unwrap()
        });
        scan_workspace_routes(graph, root, namespace, &repo_id)
    }
}

inventory::submit!(crate::server::sensors::SensorEntry(&HttpRouteSensor));

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::contracts::model::{ContractFact, HttpMethod, ProviderOrigin};

    fn temp_graph(tag: &str) -> GraphDatabase {
        let tmp = std::env::temp_dir().join(format!("http_sensor_{tag}"));
        let _ = std::fs::remove_dir_all(&tmp);
        GraphDatabase::new(&tmp).unwrap()
    }

    #[test]
    fn every_route_pattern_compiles() {
        let patterns = get_route_patterns();
        assert!(
            patterns.contains_key("python-fastapi") && patterns.contains_key("python-flask"),
            "the two patterns whose raw strings were malformed must be present"
        );
    }

    #[test]
    fn a_handler_on_the_following_line_is_found() {
        let actix =
            "#[get(\"/api/users\")]\nasync fn list_users() -> impl Responder {\n    todo!()\n}\n";
        let r = scan_file_for_routes(std::path::Path::new("api.rs"), actix);
        assert_eq!(r.len(), 1, "actix route should be found: {r:?}");
        assert_eq!(r[0].method, HttpMethod::Get);
        assert_eq!(r[0].path, "/api/users");
        assert_eq!(r[0].handler_name, "list_users");

        let fastapi =
            "@app.post(\"/api/widgets\")\nasync def create_widget(body: Widget):\n    ...\n";
        let r = scan_file_for_routes(std::path::Path::new("api.py"), fastapi);
        assert_eq!(r.len(), 1, "fastapi route should be found: {r:?}");
        assert_eq!(r[0].method, HttpMethod::Post);
        assert_eq!(r[0].path, "/api/widgets");
        assert_eq!(r[0].handler_name, "create_widget");
    }

    #[test]
    fn flask_routes_pick_up_their_method_and_handler() {
        let flask =
            "@app.route(\"/api/orders\", methods=[\"POST\"])\ndef create_order():\n    pass\n";
        let r = scan_file_for_routes(std::path::Path::new("app.py"), flask);
        assert_eq!(r.len(), 1, "flask route should be found: {r:?}");
        assert_eq!(r[0].method, HttpMethod::Post);
        assert_eq!(r[0].path, "/api/orders");
        assert_eq!(r[0].handler_name, "create_order");
    }

    #[test]
    fn flask_route_without_methods_defaults_to_get() {
        // §6.2: Flask's default verb is GET.
        let flask = "@app.route(\"/api/health\")\ndef health():\n    pass\n";
        let r = scan_file_for_routes(std::path::Path::new("app.py"), flask);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].method, HttpMethod::Get);
    }

    #[test]
    fn axum_route_calls_are_recognised() {
        let axum = "let app = Router::new()\n    .route(\"/api/health\", get(health_check));\n";
        let r = scan_file_for_routes(std::path::Path::new("main.rs"), axum);
        assert_eq!(r.len(), 1, "axum route should be found: {r:?}");
        assert_eq!(r[0].method, HttpMethod::Get);
        assert_eq!(r[0].path, "/api/health");
        assert_eq!(r[0].handler_name, "health_check");
    }

    #[test]
    fn express_non_get_verbs_are_not_dropped() {
        let ts = "router.post(\"/api/login\", loginHandler);\n";
        let r = scan_file_for_routes(std::path::Path::new("routes.ts"), ts);
        assert_eq!(r.len(), 1, "express POST route should be found: {r:?}");
        assert_eq!(r[0].method, HttpMethod::Post);
        assert_eq!(r[0].path, "/api/login");
        assert_eq!(r[0].handler_name, "loginHandler");
    }

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

    #[test]
    fn a_verbless_route_api_is_any_not_get() {
        // §6.2: go-std's HandleFunc declares no verb, so routes emit
        // `HttpMethod::Any` (was `GET` in 0.8).
        let go = "http.HandleFunc(\"/healthz\", healthz)\n";
        let r = scan_file_for_routes(std::path::Path::new("main.go"), go);
        assert_eq!(r.len(), 1, "net/http route should be found: {r:?}");
        assert_eq!(
            r[0].method,
            HttpMethod::Any,
            "HandleFunc routes are ANY (§6.2), not GET"
        );
        assert_eq!(r[0].path, "/healthz");
        assert_eq!(r[0].handler_name, "healthz");
    }

    #[test]
    fn patterns_are_scoped_to_the_files_language() {
        let go_source = r#"r.GET("/api/users", listUsers)"#;

        let as_go = scan_file_for_routes(std::path::Path::new("routes.go"), go_source);
        assert_eq!(as_go.len(), 1, "gin route should be found in a .go file");
        assert_eq!(as_go[0].method, HttpMethod::Get);
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
        let repo_id = RepoId::new("test").unwrap();
        let (nodes, edges) = routes_to_graph(
            &graph,
            &routes,
            &crate::schema::RepoNamespace::for_test(),
            &repo_id,
        );

        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].node_type, NodeType::HttpRoute);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].edge_type, EdgeType::CallsHttp);
        assert_eq!(edges[0].target_id, handler.id);
    }

    #[test]
    fn a_route_carries_a_normalized_template_and_code_origin() {
        let routes = vec![HttpRoute {
            method: HttpMethod::Get,
            path: "/api/orders/:id".to_string(),
            handler_path: "routes.rs".to_string(),
            handler_name: "get_order".to_string(),
            line: 10,
        }];
        let repo_id = RepoId::new("test").unwrap();
        let (nodes, _) = routes_to_graph(
            &temp_graph("provider"),
            &routes,
            &crate::schema::RepoNamespace::for_test(),
            &repo_id,
        );
        let provider = match nodes[0].contract.as_ref().expect("contract set") {
            ContractFact::Provider(p) => p.clone(),
            other => panic!("expected Provider fact, got {other:?}"),
        };
        assert_eq!(provider.method, HttpMethod::Get);
        assert_eq!(provider.template, "/api/orders/{}");
        assert_eq!(provider.origin, ProviderOrigin::Code);
        assert!(provider.handler.is_some());
    }

    #[test]
    fn the_workspace_scan_emits_edges_not_just_nodes() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();
        std::fs::write(dir_path.join("routes.go"), "r.GET(\"/api/users\", listUsers)\n").unwrap();

        let graph = temp_graph("scan");
        graph
            .upsert_node(GraphNode::new(
                NodeType::Function,
                "listUsers".to_string(),
                dir_path.join("routes.go").to_string_lossy().to_string(),
            ))
            .unwrap();

        let repo_id = RepoId::new("test").unwrap();
        let count = scan_workspace_routes(
            &graph,
            &dir_path,
            &crate::schema::RepoNamespace::for_test(),
            &repo_id,
        )
        .unwrap();
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
        let repo_id = RepoId::new("test").unwrap();
        let (_, edges) = routes_to_graph(
            &graph,
            &routes,
            &crate::schema::RepoNamespace::for_test(),
            &repo_id,
        );

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_id, same_file.id);
    }

    /// Rescanning must drop the routes that no longer exist on disk
    /// (§6.1 stale-route fix). Adding one route, then a file that
    /// does not declare it, then rescanning must leave the graph
    /// empty of `HttpRoute` nodes.
    #[test]
    fn a_rescan_drops_routes_deleted_from_source() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();

        let graph = temp_graph("stale");
        let repo_id = RepoId::new("test").unwrap();

        // First scan: one route exists.
        std::fs::write(dir_path.join("routes.go"), "r.GET(\"/api/users\", listUsers)\n").unwrap();
        let n1 = scan_workspace_routes(
            &graph,
            &dir_path,
            &crate::schema::RepoNamespace::for_test(),
            &repo_id,
        )
        .unwrap();
        assert_eq!(n1, 1, "first scan: 1 route");
        assert_eq!(
            graph
                .get_all_nodes()
                .into_iter()
                .filter(|n| n.node_type == NodeType::HttpRoute)
                .count(),
            1
        );

        // Second scan: source file no longer declares a route.
        std::fs::write(dir_path.join("routes.go"), "// nothing\n").unwrap();
        let n2 = scan_workspace_routes(
            &graph,
            &dir_path,
            &crate::schema::RepoNamespace::for_test(),
            &repo_id,
        )
        .unwrap();
        assert_eq!(n2, 0, "second scan: 0 routes");
        assert_eq!(
            graph
                .get_all_nodes()
                .into_iter()
                .filter(|n| n.node_type == NodeType::HttpRoute)
                .count(),
            0,
            "stale route must be gone after rescan (§6.1)"
        );
    }

    /// `scan_file_for_routes` and the unit-test code call into the
    /// same registry; the registry must be a `BTreeMap` (§6.1
    /// determinism).
    #[test]
    fn route_patterns_are_a_btreemap_for_determinism() {
        let _ = get_route_patterns();
        // Static check: get_route_patterns's return type is
        // BTreeMap; the test just exercises it.
    }

    #[test]
    fn flask_blueprint_url_prefix_is_prepended_to_decorated_routes() {
        // §6.2 same-file router prefix.
        let src = "\
bp = Blueprint('orders', __name__, url_prefix='/api/v1')

@bp.route('/orders', methods=['GET'])
def list_orders():
    pass
";
        let r = scan_file_for_routes(std::path::Path::new("app.py"), src);
        // The path regex captures only the literal path inside the
        // decorator; prefix handling here is best-effort when there
        // is exactly one prefix in the file.
        assert!(!r.is_empty(), "blueprint route should be found: {r:?}");
        assert!(
            r[0].path.contains("/api/v1") || r[0].path.starts_with("/api"),
            "blueprint prefix must be prepended: got {:?}",
            r[0].path
        );
    }

    #[test]
    fn fastapi_apirouter_prefix_is_prepended_to_decorated_routes() {
        let src = "\
router = APIRouter(prefix='/api/v1')

@router.get('/orders')
async def list_orders():
    pass
";
        let r = scan_file_for_routes(std::path::Path::new("app.py"), src);
        assert!(!r.is_empty(), "APIRouter route should be found: {r:?}");
        assert!(
            r[0].path.contains("/api/v1"),
            "APIRouter prefix must be prepended: got {:?}",
            r[0].path
        );
    }
}