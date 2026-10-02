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
//!
//! Data-driven patterns (Task 2 of the sensor-patterns plan):
//! route frameworks are listed in `frameworks.yaml` and reached
//! through `Patterns::route_patterns(Lang)`. The framework-specific
//! regex strings — which the YAML schema can't express — live in
//! `route_pattern_for` / `method_capture_for`. Adding a new
//! framework is a YAML change plus, when the framework's call shape
//! needs a custom verb capture, one match arm in
//! `method_capture_for`.

use crate::error::LainError;
use crate::federation::contracts::model::{HttpMethod, ProviderFact, ProviderOrigin, SymbolKey};
use crate::federation::contracts::normalize::{normalize, UrlPart};
use crate::federation::repo_id::RepoId;
use crate::graph::{GraphDatabase, SensorOwner};
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
use crate::server::sensors::patterns::{Patterns, RoutePattern};
use crate::server::sensors::util::{parse_for_lang, Lang};
use std::cell::Cell;
use std::collections::BTreeMap;
use tree_sitter::{StreamingIterator, Tree};

// Test-only instrumentation for the per-call-loop hoist. The
// counter is incremented exactly once per `scan_file_for_routes`
// call when tree-sitter parsing is invoked on the calling thread.
// Each test thread sees its own counter (no global state), so
// concurrent sensor tests cannot false-positive the assertion.
thread_local! {
    pub(crate) static SCAN_PARSE_COUNT: Cell<usize> = const { Cell::new(0) };
}

/// Current value of the per-thread parse counter. Tests read
/// `before` and `after` around a scan call to compute the delta.
pub fn scan_parse_count() -> usize {
    SCAN_PARSE_COUNT.with(|c| c.get())
}

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

/// Extract routes from `content` for one [`RoutePattern`].
///
/// The `RoutePattern` struct lives in `crate::server::sensors::patterns`
/// (the data-driven patterns module) so the cache that stores
/// `BTreeMap<String, RoutePattern>` can sit on `Patterns` itself
/// without circular imports. The extraction walker stays here
/// because it depends on `HttpRoute` (a sensor-local type).
fn extract_routes(pattern: &RoutePattern, content: &str, file_path: &str) -> Vec<HttpRoute> {
    const HANDLER_LOOKAHEAD: usize = 6;

    let lines: Vec<&str> = content.lines().collect();
    let mut routes = Vec::new();

    for (idx, line) in lines.iter().enumerate() {
        let Some(path) = pattern
            .path_regex()
            .captures(line)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
        else {
            continue;
        };
        if path.is_empty() {
            continue;
        }

        let method = pattern
            .method_regex()
            .and_then(|re| re.captures(line))
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_uppercase())
            .map(method_from_str)
            .unwrap_or_else(|| pattern.default_method());

        // Prefer a handler on the declaring line (Gin, Express);
        // otherwise look ahead for the function it decorates
        // (Actix, FastAPI, Flask).
        let mut handler = pattern
            .handler_fn_regex()
            .captures(line)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());

        if handler.is_none() {
            for look in lines.iter().skip(idx + 1).take(HANDLER_LOOKAHEAD) {
                if let Some(h) = pattern
                    .handler_fn_regex()
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
///
/// Task 2 of the data-driven sensor-patterns plan wired this to
/// consume `Patterns::route_patterns(Lang)` (built in Task 1) instead
/// of an inline `BTreeMap`. The framework-specific regex strings
/// live in `crate::server::sensors::patterns` (moved out of
/// `http_sensor` in the runtime-override wire-in so the cache can
/// sit on `Patterns` without circular imports).
///
/// The cache itself lives on the `Patterns` instance (see
/// [`crate::server::sensors::patterns::Patterns::route_patterns_map`]).
/// Storing it on `Patterns` — keyed by the instance, not by raw
/// pointer — closes the leak-across-scans bug where two
/// consecutive scans happened to allocate their
/// `Patterns::with_overrides` clones at the same address and the
/// second scan inherited the first scan's override-augmented route
/// map. The cache is invalidated on every `load_overrides` so a
/// second load (a different `<root>/.lain/patterns/`) rebuilds the
/// map with the new override data.
///
/// `pub` so integration tests can pin the cache contract
/// (no per-file rebuild → no per-file leak for the default
/// patterns-instance).
pub fn get_route_patterns(patterns: &Patterns) -> &BTreeMap<String, RoutePattern> {
    patterns.route_patterns_map()
}

/// Scan a file for HTTP routes, after applying same-file router
/// prefixes (§6.2).
pub fn scan_file_for_routes(
    path: &std::path::Path,
    content: &str,
    patterns: &Patterns,
) -> Vec<HttpRoute> {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    let all_patterns = get_route_patterns(patterns);
    let prefixes: &[&str] = match extension {
        "rs" => &["rust-"],
        "py" => &["python-"],
        "ts" | "tsx" | "js" | "jsx" => &["tsjs-"],
        "go" => &["go-"],
        "java" => &["java-"],
        "cs" => &["csharp-"],
        "rb" => &["ruby-"],
        "kt" | "kts" => &["kotlin-"],
        _ => return Vec::new(),
    };
    let applicable: Vec<(&str, &RoutePattern)> = all_patterns
        .iter()
        .filter(|(k, _)| prefixes.iter().any(|p| k.starts_with(p)))
        .map(|(k, v)| (k.as_str(), v))
        .collect();

    let mut all_routes = Vec::new();
    let router_prefixes = extract_router_prefixes(content, extension);

    // Parse the file once up front. `lang_for_yaml_key` (used by
    // `try_treesitter_extract`) maps the prefix back to a `Lang`
    // — we use the same mapping here so the parsed tree matches the
    // grammar the `.scm` query is compiled against. A file with N
    // applicable frameworks now parses exactly once instead of N
    // times. The `SCAN_PARSE_COUNT` instrumentation lets tests pin
    // this contract on a per-thread basis (concurrent tests cannot
    // false-positive the count).
    //
    // `applicable.first()` is load-bearing here: each entry in the
    // `prefixes` slice maps to exactly one lang bucket (`rust-` →
    // `Lang::Rust`, `tsjs-` → `Lang::TsJs`, …), so the first
    // applicable entry's prefix is the file's lang. A future change
    // that allows one extension to map to multiple lang buckets
    // (e.g. `.kt` → `["kotlin-", "kotlin-ng-"]`) would need to
    // re-think this — the per-file parse would either need to fan
    // out across grammars (expensive) or pick one canonical lang
    // (lossy). Today the slice is one-prefix-per-lang, so `.first()`
    // is correct by construction.
    let parsed_tree: Option<Tree> = applicable
        .first()
        .map(|(key, _)| key)
        .and_then(|key| key.split_once('-').map(|(prefix, _)| prefix))
        .and_then(lang_for_yaml_key)
        .and_then(|lang| {
            SCAN_PARSE_COUNT.with(|c| c.set(c.get() + 1));
            parse_for_lang(lang, content)
        });

    // Per-framework regex extraction — the primary path. The
    // `BTreeMap` keeps iteration deterministic across runs.
    for (key, pattern) in &applicable {
        let routes = extract_routes(pattern, content, &path.to_string_lossy());
        for mut r in routes {
            if let Some(prefix) = router_prefix_for_receiver(&r, &router_prefixes) {
                r.path = join_prefix(prefix.as_str(), &r.path);
            }
            all_routes.push(r);
        }
        // Tree-sitter supplementary path (Task 2 §3): for each
        // framework whose `.scm` body is non-empty, run the
        // compiled query and union any new (method, path) pairs
        // the regex missed. An empty body (just a comment line)
        // skips tree-sitter entirely. The pre-parsed tree is
        // reused — `parse_for_lang` ran exactly once above.
        if let Some(ts) = try_treesitter_extract(patterns, key, content, path, parsed_tree.as_ref())
        {
            for mut r in ts {
                if let Some(prefix) = router_prefix_for_receiver(&r, &router_prefixes) {
                    r.path = join_prefix(prefix.as_str(), &r.path);
                }
                all_routes.push(r);
            }
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

/// Run the tree-sitter query for one (lang, framework) pair, if its
/// `.scm` body is non-empty. Captures follow the spec's standardised
/// names — `@path` (string literal node), `@verb` (HTTP verb token),
/// `@handler` (handler function name). Anything else is ignored and
/// the regex path remains authoritative.
///
/// `tree` is the file's pre-parsed tree (built once per scan in
/// [`scan_file_for_routes`]); the caller hoists
/// [`parse_for_lang`] out of the per-framework loop so a file with
/// N applicable frameworks parses exactly once.
fn try_treesitter_extract(
    patterns: &Patterns,
    pattern_key: &str,
    content: &str,
    path: &std::path::Path,
    tree: Option<&Tree>,
) -> Option<Vec<HttpRoute>> {
    let (lang_yaml, framework) = pattern_key.split_once('-')?;
    let lang = lang_for_yaml_key(lang_yaml)?;
    let tree = tree?;

    // The compiled queries use `<lang>/<framework>.scm`. Resolve
    // the framework id by stripping the `-route` / `-outbound` /
    // etc. tail; for now only `*-route` frameworks are mapped.
    let framework_id = framework.strip_suffix("-route")?;
    let scm_key = format!("{lang_yaml}/{framework_id}-route.scm");

    let body = patterns
        .compiled_queries()
        .ok()?
        .iter()
        .find(|(k, _, _, _)| *k == scm_key)
        .map(|(_, _, _, body)| *body)?;

    // Empty body or comment-only — the brief's fallback case.
    let has_query = body.lines().any(|line| {
        let trimmed = line.trim_start();
        !trimmed.is_empty() && !trimmed.starts_with(';')
    });
    if !has_query {
        return None;
    }

    let grammar = crate::server::sensors::util::language_for(lang);
    let query = tree_sitter::Query::new(&grammar, body).ok()?;
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

    let mut routes = Vec::new();
    let mut path_idx: Option<u32> = None;
    let mut verb_idx: Option<u32> = None;
    let mut handler_idx: Option<u32> = None;
    for (i, name) in query.capture_names().iter().enumerate() {
        match *name {
            "path" => path_idx = Some(i as u32),
            "verb" => verb_idx = Some(i as u32),
            "handler" => handler_idx = Some(i as u32),
            _ => {}
        }
    }

    while let Some(m) = matches.next() {
        let path_text = path_idx
            .and_then(|i| m.nodes_for_capture_index(i).next())
            .and_then(|n| text_for_node(n, content));
        let verb_text = verb_idx
            .and_then(|i| m.nodes_for_capture_index(i).next())
            .and_then(|n| text_for_node(n, content));
        let handler_text = handler_idx
            .and_then(|i| m.nodes_for_capture_index(i).next())
            .and_then(|n| text_for_node(n, content));
        let (Some(path_text), Some(handler_text)) = (path_text, handler_text) else {
            continue;
        };

        let method = verb_text
            .map(|v| method_from_str(v.to_uppercase()))
            .unwrap_or(HttpMethod::Any);

        let line = path_idx
            .and_then(|i| m.nodes_for_capture_index(i).next())
            .map(|n| n.start_position().row as u32 + 1)
            .unwrap_or(1);

        routes.push(HttpRoute {
            method,
            path: strip_quotes(path_text),
            handler_path: path.to_string_lossy().into_owned(),
            handler_name: handler_text.to_string(),
            line,
        });
    }

    Some(routes)
}

/// Map the http_sensor pattern key's lang prefix to the
/// tree-sitter [`Lang`] variant. `tsjs` covers `.ts`, `.tsx`, `.js`,
/// `.jsx` via `lang_for_path`, but the walker only knows the
/// abstract [`Lang`].
fn lang_for_yaml_key(key: &str) -> Option<Lang> {
    match key {
        "rust" => Some(Lang::Rust),
        "python" => Some(Lang::Python),
        "tsjs" => Some(Lang::TsJs),
        "go" => Some(Lang::Go),
        "java" => Some(Lang::Java),
        "csharp" => Some(Lang::CSharp),
        "ruby" => Some(Lang::Ruby),
        "kotlin" => Some(Lang::Kotlin),
        _ => None,
    }
}

/// Extract the source text covered by `node` from `content`.
fn text_for_node<'a>(node: tree_sitter::Node, content: &'a str) -> Option<&'a str> {
    let start = node.start_byte();
    let end = node.end_byte();
    content.get(start..end)
}

/// Strip a single pair of surrounding quotes (`"` or `'`) from a
/// string-literal capture. The .scm queries bind `@path` to the
/// raw `string_literal` node, which carries its delimiters; the
/// walker needs the unwrapped content to match the regex path's
/// behaviour (which already strips quotes).
fn strip_quotes(s: &str) -> String {
    let trimmed = s.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

// ─── Same-file router prefixes (§6.2) ─────────────────────────────────

/// Per-receiver router prefix detected in the same file. Receiver
/// name → path prefix to prepend to that receiver's decorated routes.
fn extract_router_prefixes(content: &str, extension: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();

    if extension == "py" {
        // FastAPI: `router = APIRouter(prefix="/api/v1")`
        let re = regex::Regex::new(r#"(\w+)\s*=\s*APIRouter\s*\(\s*prefix\s*=\s*["']([^"']+)["']"#)
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
        let def_re = regex::Regex::new(r"let\s+(\w+)\s*=\s*Router::new").unwrap();
        let nest_re =
            regex::Regex::new(r#"\.nest\s*\(\s*["']([^"']+)["']\s*,\s*(\w+)\s*\)"#).unwrap();
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
            out.insert(format!("scope:{}", &scope_cap[1]), scope_cap[1].to_string());
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
///
/// Layering per-repo overrides from `<root>/.lain/patterns/` happens
/// at the top of this function via [`Patterns::with_overrides`]: the
/// per-repo YAML + `.scm` overrides are merged into the bundled
/// registry before any walker code runs, so the walker sees
/// override-augmented `Patterns::route_patterns` and
/// `Patterns::compiled_queries` data without each walker function
/// having to call `load_overrides` itself.
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
    let patterns = Patterns::with_overrides(root)?;
    let patterns: &Patterns = &patterns;

    for entry in crate::server::sensors::util::walk_workspace(root) {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        if ![
            "rs", "py", "ts", "js", "go", "java", "cs", "rb", "kt", "kts",
        ]
        .contains(&ext)
        {
            continue;
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue, // a vanished file between walk and read is fine
        };
        let mut routes = scan_file_for_routes(path, &content, patterns);
        for r in &mut routes {
            r.handler_path = crate::graph::graph_path(root, std::path::Path::new(&r.handler_path));
        }
        let (nodes, edges) = routes_to_graph(graph, &routes, namespace, repo_id);
        all_nodes.extend(nodes);
        all_edges.extend(edges);
    }

    let removed = graph.replace_sensor_output(SensorOwner::HttpSensor, &all_nodes, &all_edges)?;
    if removed > 0 {
        tracing::debug!("http_sensor: replaced {removed} stale route(s) for {root:?}");
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
        let patterns = get_route_patterns(Patterns::patterns());
        assert!(
            patterns.contains_key("python-fastapi-route")
                && patterns.contains_key("python-flask-route"),
            "the two patterns whose raw strings were malformed must be present"
        );
    }

    #[test]
    fn a_handler_on_the_following_line_is_found() {
        let actix =
            "#[get(\"/api/users\")]\nasync fn list_users() -> impl Responder {\n    todo!()\n}\n";
        let r = scan_file_for_routes(std::path::Path::new("api.rs"), actix, Patterns::patterns());
        assert_eq!(r.len(), 1, "actix route should be found: {r:?}");
        assert_eq!(r[0].method, HttpMethod::Get);
        assert_eq!(r[0].path, "/api/users");
        assert_eq!(r[0].handler_name, "list_users");

        let fastapi =
            "@app.post(\"/api/widgets\")\nasync def create_widget(body: Widget):\n    ...\n";
        let r = scan_file_for_routes(
            std::path::Path::new("api.py"),
            fastapi,
            Patterns::patterns(),
        );
        assert_eq!(r.len(), 1, "fastapi route should be found: {r:?}");
        assert_eq!(r[0].method, HttpMethod::Post);
        assert_eq!(r[0].path, "/api/widgets");
        assert_eq!(r[0].handler_name, "create_widget");
    }

    #[test]
    fn flask_routes_pick_up_their_method_and_handler() {
        let flask =
            "@app.route(\"/api/orders\", methods=[\"POST\"])\ndef create_order():\n    pass\n";
        let r = scan_file_for_routes(std::path::Path::new("app.py"), flask, Patterns::patterns());
        assert_eq!(r.len(), 1, "flask route should be found: {r:?}");
        assert_eq!(r[0].method, HttpMethod::Post);
        assert_eq!(r[0].path, "/api/orders");
        assert_eq!(r[0].handler_name, "create_order");
    }

    #[test]
    fn flask_route_without_methods_defaults_to_get() {
        // §6.2: Flask's default verb is GET.
        let flask = "@app.route(\"/api/health\")\ndef health():\n    pass\n";
        let r = scan_file_for_routes(std::path::Path::new("app.py"), flask, Patterns::patterns());
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].method, HttpMethod::Get);
    }

    #[test]
    fn axum_route_calls_are_recognised() {
        let axum = "let app = Router::new()\n    .route(\"/api/health\", get(health_check));\n";
        let r = scan_file_for_routes(std::path::Path::new("main.rs"), axum, Patterns::patterns());
        assert_eq!(r.len(), 1, "axum route should be found: {r:?}");
        assert_eq!(r[0].method, HttpMethod::Get);
        assert_eq!(r[0].path, "/api/health");
        assert_eq!(r[0].handler_name, "health_check");
    }

    #[test]
    fn express_non_get_verbs_are_not_dropped() {
        let ts = "router.post(\"/api/login\", loginHandler);\n";
        let r = scan_file_for_routes(std::path::Path::new("routes.ts"), ts, Patterns::patterns());
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
            let r =
                scan_file_for_routes(std::path::Path::new("routes.go"), src, Patterns::patterns());
            assert_eq!(r.len(), 1, "gin route should be found in {src:?}: {r:?}");
            assert_eq!(r[0].handler_name, "listUsers");
        }
    }

    /// Echo routes use the same shape as Gin (`e.GET("/path", h)`);
    /// the existing gin regex covers them.
    #[test]
    fn echo_routes_are_covered_by_the_gin_regex() {
        for src in [
            "e.GET(\"/api/users\", listUsers)\n",
            "e.POST(\"/api/login\", authHandler)\n",
        ] {
            let r =
                scan_file_for_routes(std::path::Path::new("routes.go"), src, Patterns::patterns());
            assert_eq!(r.len(), 1, "echo route should be found in {src:?}: {r:?}");
            assert!(r[0].method != HttpMethod::Any, "echo carries a verb: {r:?}");
        }
    }

    #[test]
    fn a_declaration_does_not_claim_a_distant_function() {
        let mut src = String::from("#[get(\"/api/users\")]\n");
        for _ in 0..12 {
            src.push_str("// filler\n");
        }
        src.push_str("async fn much_later() {}\n");
        let r = scan_file_for_routes(std::path::Path::new("api.rs"), &src, Patterns::patterns());
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
        let r = scan_file_for_routes(std::path::Path::new("main.go"), go, Patterns::patterns());
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

        let as_go = scan_file_for_routes(
            std::path::Path::new("routes.go"),
            go_source,
            Patterns::patterns(),
        );
        assert_eq!(as_go.len(), 1, "gin route should be found in a .go file");
        assert_eq!(as_go[0].method, HttpMethod::Get);
        assert_eq!(as_go[0].path, "/api/users");
        assert_eq!(as_go[0].handler_name, "listUsers");

        let as_python = scan_file_for_routes(
            std::path::Path::new("routes.py"),
            go_source,
            Patterns::patterns(),
        );
        assert!(
            as_python.is_empty(),
            "Go route syntax must not be matched by the Python patterns: {as_python:?}"
        );

        let unknown = scan_file_for_routes(
            std::path::Path::new("notes.txt"),
            go_source,
            Patterns::patterns(),
        );
        assert!(
            unknown.is_empty(),
            "an unhandled extension yields no routes"
        );
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
            Patterns::patterns(),
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
        std::fs::write(
            dir_path.join("routes.go"),
            "r.GET(\"/api/users\", listUsers)\n",
        )
        .unwrap();

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
            Patterns::patterns(),
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
        std::fs::write(
            dir_path.join("routes.go"),
            "r.GET(\"/api/users\", listUsers)\n",
        )
        .unwrap();
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
    /// determinism). Two back-to-back calls must return equal
    /// iterators so the order of route emission is reproducible.
    ///
    /// The registry is now served from a `OnceLock` cache on the
    /// `Patterns` instance (the wire-in lifetime-tightening pass
    /// moved it from a `thread_local!` keyed on raw pointer to a
    /// per-instance field), so two back-to-back calls on the
    /// same `Patterns` return the *same* `&BTreeMap` reference —
    /// the test pins both pointer-equality (the cache is
    /// single-instance) and the BTreeMap's sorted iteration
    /// order.
    #[test]
    fn route_patterns_are_a_btreemap_for_determinism() {
        let first_ptr = get_route_patterns(Patterns::patterns()) as *const _;
        let second_ptr = get_route_patterns(Patterns::patterns()) as *const _;
        assert_eq!(
            first_ptr, second_ptr,
            "two back-to-back calls must return the same &'_ reference (OnceLock cache)",
        );
        let first: Vec<String> = get_route_patterns(Patterns::patterns())
            .keys()
            .cloned()
            .collect();
        let second: Vec<String> = get_route_patterns(Patterns::patterns())
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            first, second,
            "two back-to-back calls must produce the same key order (BTreeMap §6.1 determinism)",
        );
        // A `BTreeMap` iterates in sorted order, so the keys must be
        // monotonically increasing. A future regression that swaps
        // the map type for `HashMap` (and reorders iteration) would
        // fail here even if `first == second` happened to hold by
        // coincidence in the same process.
        let mut sorted = first.clone();
        sorted.sort_unstable();
        assert_eq!(
            first, sorted,
            "BTreeMap iteration must be in sorted order; got {first:?}",
        );
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
        let r = scan_file_for_routes(std::path::Path::new("app.py"), src, Patterns::patterns());
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
        let r = scan_file_for_routes(std::path::Path::new("app.py"), src, Patterns::patterns());
        assert!(!r.is_empty(), "APIRouter route should be found: {r:?}");
        assert!(
            r[0].path.contains("/api/v1"),
            "APIRouter prefix must be prepended: got {:?}",
            r[0].path
        );
    }

    // ─── Workstream 5: Java Spring, JAX-RS, ASP.NET, Sinatra, Rails, Ktor.

    #[test]
    fn java_spring_getmapping_is_recognised() {
        let src = "\
@RestController
public class Foo {
    @GetMapping(\"/api/users\")
    public String listUsers() {
        return \"\";
    }
}
";
        let r = scan_file_for_routes(std::path::Path::new("Foo.java"), src, Patterns::patterns());
        assert_eq!(r.len(), 1, "Spring @GetMapping must be detected: {r:?}");
        assert_eq!(r[0].method, HttpMethod::Get);
        assert_eq!(r[0].path, "/api/users");
        assert_eq!(r[0].handler_name, "listUsers");
    }

    #[test]
    fn java_spring_postmapping_is_recognised() {
        let src = "\
@RestController
public class Foo {
    @PostMapping(value = \"/api/users\")
    public String createUser() { return \"\"; }
}
";
        let r = scan_file_for_routes(std::path::Path::new("Foo.java"), src, Patterns::patterns());
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].method, HttpMethod::Post);
    }

    #[test]
    fn java_jaxrs_get_on_method_is_recognised() {
        let src = "\
@Path(\"/api\")
public class Foo {
    @GET
    @Path(\"/users\")
    public String list() { return \"\"; }
}
";
        let r = scan_file_for_routes(std::path::Path::new("Foo.java"), src, Patterns::patterns());
        // The regex-based extractor detects the method-level `@GET`
        // and the class-level `@Path`. v1 emits one route per
        // detected annotation pair.
        assert!(!r.is_empty(), "JAX-RS @GET must be detected: {r:?}");
    }

    #[test]
    fn java_non_route_code_is_ignored() {
        let src = "public class Foo { @Override public String toString() { return \"\"; } }\n";
        let r = scan_file_for_routes(std::path::Path::new("Foo.java"), src, Patterns::patterns());
        assert!(r.is_empty(), "@Override must not be a route: {r:?}");
    }

    #[test]
    fn csharp_aspnet_httpget_is_recognised() {
        let src =
            "[HttpGet(\"/api/users/{id}\")]\npublic IActionResult Get(int id) { return null; }\n";
        let r = scan_file_for_routes(std::path::Path::new("Foo.cs"), src, Patterns::patterns());
        assert_eq!(r.len(), 1, "ASP.NET [HttpGet] must be detected: {r:?}");
        assert_eq!(r[0].method, HttpMethod::Get);
        assert_eq!(r[0].path, "/api/users/{id}");
        assert_eq!(r[0].handler_name, "Get");
    }

    #[test]
    fn csharp_aspnet_httppost_is_recognised() {
        let src = "\
public class FooController : Controller {
    [HttpPost(\"/api/users\")]
    public IActionResult Post() { return null; }
}
";
        let r = scan_file_for_routes(std::path::Path::new("Foo.cs"), src, Patterns::patterns());
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].method, HttpMethod::Post);
    }

    #[test]
    fn csharp_minimal_api_mapget_is_recognised() {
        let src = "var builder = WebApplication.CreateBuilder(args);\nvar app = builder.Build();\napp.MapGet(\"/api/health\", () => \"ok\");\n";
        let r = scan_file_for_routes(
            std::path::Path::new("Program.cs"),
            src,
            Patterns::patterns(),
        );
        // The handler regex matches `() => "ok"`. The `MapGet`
        // pattern fires once.
        assert!(!r.is_empty(), "Minimal API MapGet must be detected: {r:?}");
    }

    #[test]
    fn ruby_sinatra_get_block_is_recognised() {
        let src = "\
get '/hello' do
  'Hello World'
end
";
        let r = scan_file_for_routes(std::path::Path::new("app.rb"), src, Patterns::patterns());
        assert_eq!(r.len(), 1, "Sinatra get block must be detected: {r:?}");
        assert_eq!(r[0].method, HttpMethod::Get);
        assert_eq!(r[0].path, "/hello");
    }

    #[test]
    fn ruby_sinatra_post_block_is_recognised() {
        let src = "post '/users' do\n  User.create(params)\nend\n";
        let r = scan_file_for_routes(std::path::Path::new("app.rb"), src, Patterns::patterns());
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].method, HttpMethod::Post);
        assert_eq!(r[0].path, "/users");
    }

    #[test]
    fn ruby_rails_routes_are_recognised() {
        let src = "\
Rails.application.routes.draw do
  get 'health' => 'health#show'
  resources :users
end
";
        let r = scan_file_for_routes(
            std::path::Path::new("config/routes.rb"),
            src,
            Patterns::patterns(),
        );
        // The simple verb+path shape (`get 'health'`) is detected;
        // `resources :users` is out of scope for v1 (the regex
        // doesn't capture a path there).
        let verbs: Vec<HttpMethod> = r.iter().map(|x| x.method).collect();
        assert!(
            verbs.contains(&HttpMethod::Get),
            "Rails get route must be detected: {r:?}"
        );
    }

    #[test]
    fn kotlin_ktor_get_block_is_recognised() {
        let src = "\
fun Application.module() {
    routing {
        get(\"/api/health\") { call.respondText(\"ok\") }
    }
}
";
        let r = scan_file_for_routes(std::path::Path::new("App.kt"), src, Patterns::patterns());
        assert_eq!(r.len(), 1, "Ktor get block must be detected: {r:?}");
        assert_eq!(r[0].method, HttpMethod::Get);
        assert_eq!(r[0].path, "/api/health");
    }

    #[test]
    fn kotlin_ktor_post_block_is_recognised() {
        let src = "fun Application.module() {\n    routing { post(\"/api/users\") { call.respond(\"\") } }\n}\n";
        let r = scan_file_for_routes(std::path::Path::new("App.kt"), src, Patterns::patterns());
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].method, HttpMethod::Post);
    }

    #[test]
    fn routes_are_scoped_to_their_language_extension() {
        // Java Spring syntax in a C# file must not be detected by
        // the csharp pattern (and vice versa).
        let java_src = "@GetMapping(\"/x\")\npublic String a() { return \"\"; }\n";
        let as_cs =
            scan_file_for_routes(std::path::Path::new("x.cs"), java_src, Patterns::patterns());
        assert!(
            as_cs.is_empty(),
            "Java syntax in a .cs file must not be a C# route: {as_cs:?}"
        );

        let csharp_src = "[HttpGet(\"/x\")]\npublic IActionResult A() { return null; }\n";
        let as_java = scan_file_for_routes(
            std::path::Path::new("x.java"),
            csharp_src,
            Patterns::patterns(),
        );
        assert!(
            as_java.is_empty(),
            "C# syntax in a .java file must not be a Java route: {as_java:?}"
        );
    }
}
