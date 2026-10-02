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
use crate::server::sensors::patterns::{FrameworkDef, Patterns};
use crate::server::sensors::util::{parse_for_lang, Lang};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::OnceLock;
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

/// HTTP route patterns per language
pub struct RoutePattern {
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
///
/// Task 2 of the data-driven sensor-patterns plan wired this to
/// consume `Patterns::route_patterns(Lang)` (built in Task 1) instead
/// of an inline `BTreeMap`. The framework-specific regex strings
/// live in [`route_pattern_for`] — one override per (lang, framework)
/// — because the YAML schema can't express some framework quirks
/// (Flask's `methods=["POST"]` shape, Go-std's verbless routes).
///
/// The map is built once via [`OnceLock`] and reused across every
/// `scan_file_for_routes` call. Pre-parked-cleanup the build ran on
/// every scan, leaking ~16 `&'static str` per call (one for the
/// pattern key, one for the handler-regex override, one for the
/// method-regex override). With caching, the leaks happen exactly
/// once at first call — every subsequent call returns a `&'static`
/// reference to the cached map.
///
/// `pub` so integration tests can pin the cache contract
/// (no per-call rebuild → no per-call leak).
pub fn get_route_patterns() -> &'static BTreeMap<&'static str, RoutePattern> {
    static CACHE: OnceLock<BTreeMap<&'static str, RoutePattern>> = OnceLock::new();
    CACHE.get_or_init(build_route_patterns)
}

fn build_route_patterns() -> BTreeMap<&'static str, RoutePattern> {
    let mut patterns = BTreeMap::new();

    let registry = Patterns::patterns();

    // Walk every language the http_sensor recognises. The order
    // (sorted by lang_key, then by YAML load order inside each
    // bucket) is the determinism contract §8.3 requires; the
    // `BTreeMap` keeps the iteration order stable across runs.
    let langs: [Lang; 8] = [
        Lang::Rust,
        Lang::Python,
        Lang::TsJs,
        Lang::Go,
        Lang::Java,
        Lang::CSharp,
        Lang::Ruby,
        Lang::Kotlin,
    ];

    for lang in langs {
        for def in registry.route_patterns(lang) {
            let Some(rp) = route_pattern_for(def) else {
                continue;
            };
            let key = route_pattern_key(lang, def);
            patterns.insert(key, rp);
        }
    }

    patterns
}

/// Stable map key for a (lang, framework) pair. Mirrors the inline
/// table's `<lang-prefix>-<framework-tail>` shape so existing call
/// sites (tests, the `prefixes` slice in `scan_file_for_routes`)
/// keep matching.
fn route_pattern_key(lang: Lang, def: &FrameworkDef) -> &'static str {
    let prefix = match lang {
        Lang::Rust => "rust",
        Lang::Python => "python",
        Lang::TsJs | Lang::Ts | Lang::Tsx => "tsjs",
        Lang::Go => "go",
        Lang::Java => "java",
        Lang::CSharp => "csharp",
        Lang::Ruby => "ruby",
        Lang::Kotlin => "kotlin",
    };
    let id = def.id.clone();
    let key = format!("{prefix}-{id}");
    Box::leak(key.into_boxed_str())
}

/// Build a [`RoutePattern`] for one `FrameworkDef` from the bundled
/// YAML, layering framework-specific regex overrides that the YAML
/// schema can't encode (Flask's `methods=["POST"]` kwarg, the
/// verbless `http.HandleFunc` shape, etc.).
///
/// Contract:
///   - `Some(_)` is returned when `def.path_regex` is present; the
///     walker has something to match against and the framework is
///     emitted as a candidate route pattern.
///   - `None` is returned when `def.path_regex` is missing — the
///     walker has nothing to match against, and the framework is
///     silently skipped (the same was true of the pre-Task-2 inline
///     table: frameworks without a path regex just weren't listed).
///
/// Fields read from [`FrameworkDef`]:
///   - `def.path_regex` — the route-template regex; panics at
///     construction if it fails to compile (so a malformed YAML entry
///     crashes the binary loudly rather than corrupting the walker).
///   - `def.handler_regex` — captured by [`handler_regex_for`]; for
///     most frameworks it's used as-is, but Sinatra / Minimal API /
///     Rails need a framework-specific override (the YAML's regex
///     lost a capture group during the Task-1 data conversion).
///   - `def.verbs` — joined into a `(verb|verb|verb)` alternation
///     inside [`method_capture_for`] to build `method_regex`. The
///     escape order is the YAML's verbatim order, so framework ids
///     that put `GET` / `POST` first in `verbs` match `GET` before
///     `POST`.
///
/// The returned `RoutePattern`'s `default_method` is the framework's
/// verb when the method regex is unambiguous (`HttpMethod::Get` for
/// "verbed" frameworks) and `HttpMethod::Any` when the framework
/// admits any HTTP verb or has an empty `verbs:` list (verbless
/// APIs like Go-std `HandleFunc`).
fn route_pattern_for(def: &FrameworkDef) -> Option<RoutePattern> {
    let path_re = def.path_regex.as_deref()?;
    let handler_re = handler_regex_for(def);

    let (method_re, default) = method_capture_for(def);

    let path_regex = regex::Regex::new(path_re)
        .unwrap_or_else(|e| panic!("{}: invalid path_regex {:?}: {e}", def.id, path_re));
    let handler_fn_regex = regex::Regex::new(handler_re)
        .unwrap_or_else(|e| panic!("{}: invalid handler_regex {:?}: {e}", def.id, handler_re));
    let method_regex = method_re.map(|s| {
        regex::Regex::new(s)
            .unwrap_or_else(|e| panic!("{}: invalid method_regex {:?}: {e}", def.id, s))
    });

    Some(RoutePattern {
        method_regex,
        path_regex,
        handler_fn_regex,
        default_method: default,
    })
}

/// Resolve the handler-capture regex for `def`. Returns the YAML's
/// `handler_regex` when it has a useful capture group, or a
/// framework-specific override when the YAML regex was simplified
/// (the original inline regex had capture groups the YAML lost in
/// Task 1's data conversion).
fn handler_regex_for(def: &FrameworkDef) -> &'static str {
    match def.id.as_str() {
        // Sinatra — the inline regex captured the verb on the
        // declaring line as the handler name (`Sinatra__do_block`
        // per the §6.2 comment). YAML's `do\s*$` has no capture.
        "sinatra-route" => {
            Box::leak(
                r#"(?m)^[ \t]*(get|post|put|delete|patch|options|head)\s+['"][^'"]+['"]"#
                    .to_string()
                    .into_boxed_str(),
            )
        }
        // Minimal API — the inline regex captured the entire
        // quoted path (group 1 = `"/api/health"`). YAML's
        // simplified regex has no capture group; we restore the
        // capture here so the `RoutePattern::extract` look-ahead
        // finds a non-empty handler name on the same line.
        "minimal-api-route" => {
            Box::leak(
                r#"\.(?:MapGet|MapPost|MapPut|MapDelete|MapPatch)\s*\(\s*(['"][^'"]+['"])\s*,\s*(?:async\s*)?\([^)]*\)\s*=>"#
                    .to_string()
                    .into_boxed_str(),
            )
        }
        // Rails — no handler_regex in YAML; capture the verb (or
        // the word following the path) as the handler.
        "rails-route" => {
            Box::leak(
                r"(?m)^[ \t]*(get|post|put|patch|delete|options|head|resources)\b"
                    .to_string()
                    .into_boxed_str(),
            )
        }
        // Default — use the YAML's handler_regex as-is, or fall
        // back to a word-boundary placeholder that captures any
        // identifier on the line.
        _ => Box::leak(
            def.handler_regex
                .as_deref()
                .unwrap_or(r"\b\w+\b")
                .to_string()
                .into_boxed_str(),
        ),
    }
}

/// Per-framework `method_regex` + `default_method`. The YAML's
/// `verbs` field drives the verb list; the surrounding syntax is
/// framework-specific and lives here. Returns `None` for verbless
/// APIs (Go-std `HandleFunc`) — those default to
/// [`HttpMethod::Any`] per §6.2.
fn method_capture_for(def: &FrameworkDef) -> (Option<&'static str>, HttpMethod) {
    let verbs = def.verbs.join("|");
    match def.id.as_str() {
        // Flask — the verb lives in `methods=["POST"]`, not in the
        // `@app.route("/…")` decorator.
        "flask-route" => (Some(r#"methods\s*=\s*\[\s*["'](\w+)"#), HttpMethod::Get),
        // Go stdlib — `http.HandleFunc` declares no verb at the
        // call site; routes emit `HttpMethod::Any`.
        "stdlib-http-route" => (None, HttpMethod::Any),
        // Kotlin — `routing { get("/path") { … } }` puts the verb
        // before the parenthesised path.
        "ktor-route" => (
            Some(Box::leak(
                format!(r"(?m)(?:^|\W)({verbs})\s*\(").into_boxed_str(),
            )),
            HttpMethod::Any,
        ),
        // Rails — `get 'path' do … end` style. The verb may be
        // followed by a quote (path), a colon (resources), or
        // whitespace.
        "rails-route" => (
            Some(Box::leak(
                format!(r#"(?i:({verbs}))['"\s:]+"#).into_boxed_str(),
            )),
            HttpMethod::Any,
        ),
        // C# Minimal API — the verb is baked into `MapGet` /
        // `MapPost` etc., not a separate token.
        "minimal-api-route" => (
            Some(r"\.(?i:(MapGet|MapPost|MapPut|MapDelete|MapPatch))"),
            HttpMethod::Any,
        ),
        // C# ASP.NET controllers — `[HttpGet]` etc. with the verb
        // baked into the attribute name.
        "aspnet-route" => (
            Some(Box::leak(format!(r"(?i:\[Http({verbs}))").into_boxed_str())),
            HttpMethod::Any,
        ),
        // JAX-RS — `@GET` / `@POST` on its own line, just above
        // the method declaration.
        "jaxrs-route" => (
            Some(Box::leak(format!(r"@(?i:({verbs}))\s*$").into_boxed_str())),
            HttpMethod::Any,
        ),
        // Spring — `@GetMapping` / `@PostMapping` / etc.
        "spring-route" => (
            Some(Box::leak(
                format!(r"@(?i:({verbs}))Mapping").into_boxed_str(),
            )),
            HttpMethod::Any,
        ),
        // axum — `.route("/path", get(handler))` — verb appears
        // as the second argument to `.route()`.
        "axum-route" => (
            Some(Box::leak(
                format!(r"\.route\s*\([^,]*,\s*(?i:({verbs}))\s*\(").into_boxed_str(),
            )),
            HttpMethod::Any,
        ),
        // actix-web — `#[get("/path")]` attribute on a function.
        "actix-route" => (
            Some(Box::leak(
                format!(r"#\[(?i:({verbs}))\s*\(").into_boxed_str(),
            )),
            HttpMethod::Any,
        ),
        // FastAPI — `@app.get("/path")` decorator.
        "fastapi-route" => (
            Some(Box::leak(
                format!(r"@[\w\.]+\.({verbs})\s*\(").into_boxed_str(),
            )),
            HttpMethod::Any,
        ),
        // Sinatra — `get '/path' do … end`.
        "sinatra-route" => (
            Some(Box::leak(
                format!(r#"(?i:({verbs}))\s+['"]"#).into_boxed_str(),
            )),
            HttpMethod::Any,
        ),
        // Gin / Echo — `r.GET("/path", handler)`. The verb is
        // uppercase at the call site (verbs in the YAML are
        // already uppercase).
        "gin-route" => (
            Some(Box::leak(format!(r"\.({verbs})\s*\(").into_boxed_str())),
            HttpMethod::Any,
        ),
        // Express / Fastify — `router.post("/path", handler)`.
        "express-route" | "fastify-route" => (
            Some(Box::leak(format!(r"\.({verbs})\s*\(").into_boxed_str())),
            HttpMethod::Any,
        ),
        // Unknown framework — leave the method regex unset and
        // rely on `default_method`. Future frameworks opt in by
        // adding a match arm above.
        _ => (None, HttpMethod::Any),
    }
}

/// Scan a file for HTTP routes, after applying same-file router
/// prefixes (§6.2).
pub fn scan_file_for_routes(path: &std::path::Path, content: &str) -> Vec<HttpRoute> {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    let all_patterns = get_route_patterns();
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
    let applicable: Vec<(&'static str, &RoutePattern)> = all_patterns
        .iter()
        .filter(|(k, _)| prefixes.iter().any(|p| k.starts_with(p)))
        .map(|(k, v)| (*k, v))
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
        let routes = pattern.extract(content, &path.to_string_lossy());
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
        if let Some(ts) = try_treesitter_extract(key, content, path, parsed_tree.as_ref()) {
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

    let body = Patterns::patterns()
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
        let mut routes = scan_file_for_routes(path, &content);
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
        let patterns = get_route_patterns();
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

    /// Echo routes use the same shape as Gin (`e.GET("/path", h)`);
    /// the existing gin regex covers them.
    #[test]
    fn echo_routes_are_covered_by_the_gin_regex() {
        for src in [
            "e.GET(\"/api/users\", listUsers)\n",
            "e.POST(\"/api/login\", authHandler)\n",
        ] {
            let r = scan_file_for_routes(std::path::Path::new("routes.go"), src);
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
    /// The registry is now served from a `OnceLock` cache (Item A
    /// of the parked-cleanup pass), so two back-to-back calls
    /// return the *same* `&'static` reference — the test pins both
    /// pointer-equality (the cache is single-instance) and the
    /// BTreeMap's sorted iteration order.
    #[test]
    fn route_patterns_are_a_btreemap_for_determinism() {
        let first_ptr = get_route_patterns() as *const _;
        let second_ptr = get_route_patterns() as *const _;
        assert_eq!(
            first_ptr, second_ptr,
            "two back-to-back calls must return the same &'_ reference (OnceLock cache)",
        );
        let first: Vec<&'static str> = get_route_patterns().keys().copied().collect();
        let second: Vec<&'static str> = get_route_patterns().keys().copied().collect();
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
        let r = scan_file_for_routes(std::path::Path::new("Foo.java"), src);
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
        let r = scan_file_for_routes(std::path::Path::new("Foo.java"), src);
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
        let r = scan_file_for_routes(std::path::Path::new("Foo.java"), src);
        // The regex-based extractor detects the method-level `@GET`
        // and the class-level `@Path`. v1 emits one route per
        // detected annotation pair.
        assert!(!r.is_empty(), "JAX-RS @GET must be detected: {r:?}");
    }

    #[test]
    fn java_non_route_code_is_ignored() {
        let src = "public class Foo { @Override public String toString() { return \"\"; } }\n";
        let r = scan_file_for_routes(std::path::Path::new("Foo.java"), src);
        assert!(r.is_empty(), "@Override must not be a route: {r:?}");
    }

    #[test]
    fn csharp_aspnet_httpget_is_recognised() {
        let src =
            "[HttpGet(\"/api/users/{id}\")]\npublic IActionResult Get(int id) { return null; }\n";
        let r = scan_file_for_routes(std::path::Path::new("Foo.cs"), src);
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
        let r = scan_file_for_routes(std::path::Path::new("Foo.cs"), src);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].method, HttpMethod::Post);
    }

    #[test]
    fn csharp_minimal_api_mapget_is_recognised() {
        let src = "var builder = WebApplication.CreateBuilder(args);\nvar app = builder.Build();\napp.MapGet(\"/api/health\", () => \"ok\");\n";
        let r = scan_file_for_routes(std::path::Path::new("Program.cs"), src);
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
        let r = scan_file_for_routes(std::path::Path::new("app.rb"), src);
        assert_eq!(r.len(), 1, "Sinatra get block must be detected: {r:?}");
        assert_eq!(r[0].method, HttpMethod::Get);
        assert_eq!(r[0].path, "/hello");
    }

    #[test]
    fn ruby_sinatra_post_block_is_recognised() {
        let src = "post '/users' do\n  User.create(params)\nend\n";
        let r = scan_file_for_routes(std::path::Path::new("app.rb"), src);
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
        let r = scan_file_for_routes(std::path::Path::new("config/routes.rb"), src);
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
        let r = scan_file_for_routes(std::path::Path::new("App.kt"), src);
        assert_eq!(r.len(), 1, "Ktor get block must be detected: {r:?}");
        assert_eq!(r[0].method, HttpMethod::Get);
        assert_eq!(r[0].path, "/api/health");
    }

    #[test]
    fn kotlin_ktor_post_block_is_recognised() {
        let src = "fun Application.module() {\n    routing { post(\"/api/users\") { call.respond(\"\") } }\n}\n";
        let r = scan_file_for_routes(std::path::Path::new("App.kt"), src);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].method, HttpMethod::Post);
    }

    #[test]
    fn routes_are_scoped_to_their_language_extension() {
        // Java Spring syntax in a C# file must not be detected by
        // the csharp pattern (and vice versa).
        let java_src = "@GetMapping(\"/x\")\npublic String a() { return \"\"; }\n";
        let as_cs = scan_file_for_routes(std::path::Path::new("x.cs"), java_src);
        assert!(
            as_cs.is_empty(),
            "Java syntax in a .cs file must not be a C# route: {as_cs:?}"
        );

        let csharp_src = "[HttpGet(\"/x\")]\npublic IActionResult A() { return null; }\n";
        let as_java = scan_file_for_routes(std::path::Path::new("x.java"), csharp_src);
        assert!(
            as_java.is_empty(),
            "C# syntax in a .java file must not be a Java route: {as_java:?}"
        );
    }
}
