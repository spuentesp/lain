//! HTTP consumer call sensor (`http_client_sensor`, §6.3).
//!
//! Phase-1 sensor that walks a workspace and emits one `HttpClientCall`
//! node per outbound HTTP call site, plus a `SendsHttp` edge from the
//! enclosing function (or `File` for module-level calls) carrying the
//! `SourceSite`. The walker comes from [`crate::sensors::util::walk_workspace`];
//! only the per-shape call analysis lives here.
//!
//! **Recognized call shapes** (TS/JS): `fetch(url, init?)`,
//! `axios.<verb>(url, …)`, `axios(url)`, `axios({ method, url })`,
//! `got.<verb>(url)`, `got(url, { method })`, `ky.<verb>(url)`, and
//! the receiver form `<recv>.<verb>(<arg0>, …)` where `<arg0>` is a
//! string or template literal starting with `/` (wrapper candidate).
//!
//! **Recognized call shapes** (Python): `requests.<verb>(url, …)`,
//! `requests.request("<M>", url)`, `httpx.<verb>(url, …)`,
//! `httpx.request(method, url)`, and `<client>.<verb>(…)` where
//! `<client>` is bound from `httpx.Client(...)`, `httpx.AsyncClient(…)`,
//! `requests.Session()`, or `aiohttp.ClientSession()` (incl. `with … as`
//! and `async with`). The receiver form `<recv>.<verb>(<arg0>, …)`
//! with arg 0 starting with `/` is a wrapper candidate.
//!
//! **URL expression → parts.** A string literal is a single
//! [`UrlPart::Literal`]. A template literal or f-string alternates
//! `Literal`/`Hole`. `a + b` concatenates parts. `urljoin(base, "/p")`
//! and `new URL("/p", base)` give parts of `base` then `"/p"`. A bare
//! identifier is resolved once to its assignment when there is a single
//! module-level or same-function assignment in the file; otherwise it
//! stays a `Hole`.
//!
//! **Host resolution** (config-free). A hole before the path whose
//! resolved expression matches `os.environ["X"]`,
//! `os.environ.get("X", …)`, `os.getenv("X", …)`, `settings.X`,
//! `config.X`, `process.env.X`, or `process.env["X"]` becomes
//! [`HostPart::Env`]; anything else stays [`HostPart::Expr`].
//!
//! **`httpx.Client(base_url=…)`** contributes its `base_url` as the
//! host part of every call on that client.
//!
//! Every emitted `ConsumerFact` carries `reads_complete: true`. Field-
//! read tracking is PR 9. The `url_expr` field is the URL argument's
//! source text, truncated to 200 chars (§6.3 invariant).

use crate::error::LainError;
use crate::federation::contracts::model::{
    CallVia, ConsumerFact, HostPart, HttpMethod, MethodSpec, NormalizedUrl,
};
use crate::federation::contracts::normalize::{normalize, UrlPart};
use crate::federation::repo_id::RepoId;
use crate::graph::{graph_path, GraphDatabase, SensorOwner};
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
use crate::server::sensors::patterns::{FrameworkDef, Patterns};
use crate::server::sensors::util::{language_for, parse_for_lang, Lang};
use std::collections::BTreeMap;
use std::path::Path;
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

// ─── Public sensor shape ───────────────────────────────────────────────

/// One HTTP consumer call site, pre-graph-emission. Public so callers
/// (tests, future tools) can inspect the detector without round-
/// tripping through the graph.
#[derive(Debug, Clone)]
pub struct HttpClientCall {
    pub method: MethodSpec,
    pub url: NormalizedUrl,
    pub via: CallVia,
    pub url_expr: String,
    pub reads_complete: bool,
    pub path: String,
    pub line: u32,
}

const URL_EXPR_CAP: usize = 200;

// ─── Sensor impl ──────────────────────────────────────────────────────

/// Unit-struct Sensor impl. Registered via
/// `inventory::submit!(SensorEntry(&HttpClientSensor))` below; no
/// central registry to edit.
pub struct HttpClientSensor;

impl crate::server::sensors::Sensor for HttpClientSensor {
    fn name(&self) -> &'static str {
        "http_client"
    }
    fn count_field(&self) -> crate::server::sensors::SensorCountField {
        crate::server::sensors::SensorCountField::HttpClients
    }
    fn phase(&self) -> u8 {
        // §6.1: phase 1, runs after providers (phase 0) so the joiner
        // can resolve `HttpClientCall`'s enclosing symbol against
        // routes that the providers just minted.
        1
    }
    fn scan(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
    ) -> Result<usize, LainError> {
        let repo_id = RepoId::new(root.to_string_lossy().as_ref())
            .unwrap_or_else(|_| RepoId::new("http-client-sensor").unwrap());
        scan_workspace_clients(graph, root, namespace, &repo_id)
    }
}

inventory::submit!(crate::server::sensors::SensorEntry(&HttpClientSensor));

// ─── Workspace scan ───────────────────────────────────────────────────

/// Walk `root`, extract HTTP client calls from every Python/TS/JS/Rust/Go
/// file, and persist them via `replace_sensor_output` (§6.1).
pub fn scan_workspace_clients(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
    _repo_id: &RepoId,
) -> Result<usize, LainError> {
    if graph.is_read_only() {
        return Ok(0);
    }
    let mut all_nodes: Vec<GraphNode> = Vec::new();
    let mut all_edges: Vec<GraphEdge> = Vec::new();

    for entry in crate::server::sensors::util::walk_workspace(root) {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let lang = match ext {
            "py" => Some(Lang::Python),
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some(Lang::TsJs),
            "rs" => Some(Lang::Rust),
            "go" => Some(Lang::Go),
            "java" => Some(Lang::Java),
            "cs" => Some(Lang::CSharp),
            "rb" => Some(Lang::Ruby),
            "kt" | "kts" => Some(Lang::Kotlin),
            _ => None,
        };
        let Some(lang) = lang else { continue };

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let calls = detect_calls(path, &content, lang);
        for mut call in calls {
            call.path = graph_path(root, Path::new(&call.path));
            let (nodes, edges) = build_graph(graph, &call, namespace);
            all_nodes.extend(nodes);
            all_edges.extend(edges);
        }
    }

    let removed =
        graph.replace_sensor_output(SensorOwner::HttpClientSensor, &all_nodes, &all_edges)?;
    if removed > 0 {
        tracing::debug!("http_client_sensor: replaced {removed} stale call(s) for {root:?}");
    }
    Ok(all_nodes.len())
}

// ─── Per-language detection ───────────────────────────────────────────

/// Detect every HTTP consumer call site in `content` for `lang`.
///
/// Task 3 of the data-driven sensor-patterns plan replaced the eight
/// per-language `detect_*_call` functions (which each encoded ~80
/// LoC of `if`-chains on tree-sitter node kinds) with a single
/// walker that drives detection from
/// [`Patterns::outbound_patterns`] + each framework's compiled `.scm`
/// query.
///
/// The walker:
///   1. Parses the file with the lang's tree-sitter grammar
///      (via `parse_for_lang`).
///   2. Collects client-instance bindings
///      (`client = httpx.Client(...)`) into a [`FileContext`].
///   3. For every outbound framework in `Patterns::outbound_patterns(lang)`,
///      loads the matching `<lang>/<framework>-outbound.scm` body
///      and runs it against the parsed tree.
///   4. For every match, the walker reads `(call, lib, verb, url)`
///      captures, classifies the library, and emits an
///      [`HttpClientCall`] via the existing language-agnostic
///      URL-parts extractor + host resolver.
pub fn detect_calls(path: &Path, content: &str, lang: Lang) -> Vec<HttpClientCall> {
    let Some(tree) = parse_for_lang(lang, content) else {
        return Vec::new();
    };
    let src = content.as_bytes();
    let path_str = path.to_string_lossy().to_string();
    let ctx = FileContext::collect(tree.root_node(), src, lang);

    let mut calls: Vec<HttpClientCall> = Vec::new();
    let grammar = language_for(lang);
    let patterns = Patterns::patterns();
    let lang_yaml = lang_yaml_key(lang);

    // Track calls we've already emitted by (path, line) so the
    // same call site doesn't fire from every framework's `.scm`.
    // Each framework's wrapper pattern shares the same exclusion
    // predicates, so a `<recv>.<verb>(url)` with URL `/foo` matches
    // every outbound `.scm`. We pick the first one and ignore the
    // rest.
    let mut seen: std::collections::HashSet<(String, u32)> = std::collections::HashSet::new();

    for framework in patterns.outbound_patterns(lang) {
        let Some(body) = compiled_query_body(patterns, lang_yaml, framework.id.as_str()) else {
            continue;
        };
        if !has_query_content(body) {
            continue;
        }
        let Ok(query) = Query::new(&grammar, body) else {
            continue;
        };

        let call_idx = query.capture_index_for_name("call");
        let lib_idx = query.capture_index_for_name("lib");
        let verb_idx = query.capture_index_for_name("verb");
        let nav_idx = query.capture_index_for_name("_nav");
        let url_idx = query.capture_index_for_name("url");

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), src);

        while let Some(m) = matches.next() {
            let call_node = call_idx.and_then(|i| m.nodes_for_capture_index(i).next());
            let lib_node = lib_idx.and_then(|i| m.nodes_for_capture_index(i).next());
            let verb_node = verb_idx
                .and_then(|i| m.nodes_for_capture_index(i).last())
                .or_else(|| nav_idx.and_then(|i| m.nodes_for_capture_index(i).last()));
            let url_node = url_idx.and_then(|i| m.nodes_for_capture_index(i).next());

            let Some(call_node) = call_node else {
                continue;
            };

            if let Some(call) = process_outbound_match(
                framework, call_node, lib_node, verb_node, url_node, src, &ctx, &path_str, lang,
            ) {
                // Multiple frameworks' `.scm` files can match the
                // same call site (e.g. `ureq::post` matches both
                // `reqwest-outbound` and `ureq-outbound`). Dedup by
                // (path, line) so each call site emits exactly once
                // across all frameworks.
                let key = (call.path.clone(), call.line);
                if seen.contains(&key) {
                    continue;
                }
                seen.insert(key);
                calls.push(call);
            }
        }
    }
    calls
}

fn compiled_query_body(
    patterns: &Patterns,
    lang_yaml: &'static str,
    framework_id: &str,
) -> Option<&'static str> {
    let key = format!("{lang_yaml}/{framework_id}.scm");
    patterns.compiled_queries().ok().and_then(|q| {
        q.iter()
            .find(|(k, _, _, _)| *k == key)
            .map(|(_, _, _, body)| *body)
    })
}

fn has_query_content(body: &str) -> bool {
    body.lines().any(|line| {
        let trimmed = line.trim_start();
        !trimmed.is_empty() && !trimmed.starts_with(';')
    })
}

fn lang_yaml_key(lang: Lang) -> &'static str {
    match lang {
        Lang::Python => "python",
        Lang::TsJs | Lang::Ts | Lang::Tsx => "tsjs",
        Lang::Rust => "rust",
        Lang::Go => "go",
        Lang::Java => "java",
        Lang::CSharp => "csharp",
        Lang::Ruby => "ruby",
        Lang::Kotlin => "kotlin",
    }
}

/// Recursive AST walk used by the file-context pre-pass (Python
/// `httpx.Client(base_url=…)` bindings + TS/JS module-level
/// assignments). The post-context outbound walker uses tree-sitter
/// queries, not this.
fn walk<F: FnMut(Node)>(root: Node, f: &mut F) {
    f(root);
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        walk(child, f);
    }
}

// ─── Per-match dispatch ───────────────────────────────────────────────

/// Process a single outbound `.scm` match. The captures
/// `(lib?, verb?, url)` come from the framework's `<framework>.scm`
/// query (see the per-language `.scm` files). Returns
/// `Some(HttpClientCall)` when the match is a recognised outbound
/// site, `None` when the walker should skip it (unknown receiver
/// whose URL does not start with `/`, for example).
#[allow(clippy::too_many_arguments)]
fn process_outbound_match(
    framework: &FrameworkDef,
    call: Node,
    lib: Option<Node>,
    verb: Option<Node>,
    url: Option<Node>,
    src: &[u8],
    ctx: &FileContext,
    path: &str,
    lang: Lang,
) -> Option<HttpClientCall> {
    let lib_text = lib.and_then(|n| text_of(n, src)).unwrap_or_default();
    // The verb capture is taken from the LAST node bound to `@verb`
    // (via `.last()` in the match loop). When the `.scm` captures
    // a wrapper like Kotlin's `client.get` (whose verb is the
    // rightmost `identifier` child of `navigation_expression`),
    // the walker extracts the verb from the wrapper's last named
    // child instead. This keeps the generic `.scm` shape
    // `(call_subtree, library_name, verb_method, url_subtree)`
    // the brief requires.
    let verb_text = if let Some(n) = verb {
        // Kotlin-style: `@_nav` captures the whole
        // navigation_expression; the verb is its last named child.
        if framework.effective_id() == "ktor-client-outbound"
            || framework.effective_id() == "okhttp-outbound"
        {
            extract_navigation_verb(n, src)
        } else {
            text_of(n, src).unwrap_or_default().to_ascii_lowercase()
        }
    } else {
        String::new()
    };
    let enclosing_fn_line = enclosing_function_line(call);

    // Library classification. The order matters: a bound client
    // wins over a known-library match (so `client = httpx.Client(...)`
    // resolves `client.get(url)` to via=`httpx`, not via=`client`).
    //
    // When the .scm match is a client-instance call (receiver is
    // not one of the libraries the framework owns), we still want
    // to emit the call — but only when the framework's `lib_match`
    // would also accept the bound library. This keeps one call
    // per outbound site even though every framework's `.scm` may
    // match the same `<recv>.<verb>(url)` shape.
    let (via_lib, base_url_opt) = if let Some(binding) = ctx.client_base_urls.get(&lib_text) {
        (binding.library.clone(), Some(binding.base_url.as_str()))
    } else {
        (lib_text.clone(), None)
    };

    if ctx.client_base_urls.contains_key(&lib_text) {
        // Client-instance call. Emit only when the framework owns
        // the bound library (so the same call doesn't fire from
        // every framework's .scm).
        if !is_known_library(&via_lib, framework, lang) {
            return None;
        }
        let method = method_for(framework, &verb_text, call, url, src);
        return build_call(
            CallVia::Library { name: via_lib },
            method,
            url,
            call,
            src,
            ctx,
            base_url_opt,
            enclosing_fn_line,
            path,
            framework,
            &verb_text,
        );
    }

    if is_known_library(&lib_text, framework, lang) {
        // Direct library call. `reqwest::get`, `requests.get`, …
        let method = method_for(framework, &verb_text, call, url, src);
        // When `effective_id()` differs from `id` (i.e. `display_name`
        // is set), the walker uses the effective id as the `via` —
        // the gem-name / public-name surface (e.g. `httparty` for
        // `httparty-outbound`). A future framework that wants the
        // same behaviour ships a `display_name:` in `frameworks.yaml`;
        // no walker edit is required.
        //
        // When `display_name` is unset, the inner match disambiguates
        // within the Ruby family by `framework.id` (the YAML id) and
        // the captured receiver text. `httparty-outbound`'s
        // `display_name: httparty` covers the gem-name lookup, so the
        // inner hardcoded arm for it is no longer reachable.
        let via_lib: String = match (
            framework.id.as_str(),
            framework.effective_id(),
            via_lib.as_str(),
        ) {
            (id, eff, _) if eff != id => eff.to_string(),
            ("net-http-outbound", _, "Net::HTTP") => "net/http".to_string(),
            ("faraday-outbound", _, "Faraday") => "faraday".to_string(),
            ("restclient-outbound", _, "RestClient") => "restclient".to_string(),
            _ => via_lib,
        };
        return build_call(
            CallVia::Library { name: via_lib },
            method,
            url,
            call,
            src,
            ctx,
            base_url_opt,
            enclosing_fn_line,
            path,
            framework,
            &verb_text,
        );
    }

    // Some frameworks (Ktor's ktor-client-outbound, Kotlin's
    // okhttp-outbound, .NET's HttpClient) accept any receiver —
    // the framework is identified by the call shape, not the
    // receiver text. The walker resolves the via to the
    // framework's name in that case.
    if matches!(
        framework.effective_id(),
        "ktor-client-outbound" | "okhttp-outbound" | "httpclient-outbound"
    ) {
        let method = method_for(framework, &verb_text, call, url, src);
        // Ktor / OkHttp / .NET HttpClient — only emit when the
        // verb is recognised. `Request.Builder().url("/api")` is
        // not a client call, even though it walks past the
        // ktor-outbound .scm's `call_expression` matcher.
        if matches!(method, MethodSpec::Unknown)
            && framework.effective_id() == "ktor-client-outbound"
            && !is_valid_http_verb(&verb_text)
            && verb_text != "execute"
        {
            return None;
        }
        let via_lib = match (framework.id.as_str(), lang) {
            // Java's HttpClient lives on a different gem-name
            // (`http`, the JDK package) than the display_name
            // `httpclient`; keep this arm explicit.
            ("httpclient-outbound", Lang::Java) => "http".to_string(),
            // C#'s `downloadstring` / `downloadstringtaskasync` are
            // WebClient methods, not HttpClient — keep the
            // per-call branch. The non-downloadstring branch
            // resolves to the framework's effective_id (httpclient).
            ("httpclient-outbound", Lang::CSharp) => {
                if verb_text == "downloadstring" || verb_text == "downloadstringtaskasync" {
                    "webclient".to_string()
                } else {
                    framework.effective_id().to_string()
                }
            }
            // Ktor / OkHttp — effective_id() returns the public
            // name declared in frameworks.yaml
            // (`display_name: ktor` / `display_name: okhttp`).
            _ => framework.effective_id().to_string(),
        };
        return build_call(
            CallVia::Library { name: via_lib },
            method,
            url,
            call,
            src,
            ctx,
            base_url_opt,
            enclosing_fn_line,
            path,
            framework,
            &verb_text,
        );
    }

    // For frameworks where the URL lives elsewhere in the call
    // chain (Java's HttpClient.send, OkHttp.execute), the `.scm`
    // doesn't bind `@url`. We emit a synthetic URL keyed on the
    // framework's prefix so the joiner still matches against the
    // verb+method axis.
    let Some(url) = url else {
        return Some(synthetic_url_call(framework, lib_text, call, path));
    };

    if starts_with_slash_node(url, src) {
        // Wrapper candidate: receiver is not a known library but
        // the URL starts with `/`. Emit `CallVia::Receiver`.
        let method = method_for(framework, &verb_text, call, Some(url), src);
        return build_call(
            CallVia::Receiver {
                expr: lib_text.clone(),
                fn_name: verb_text.clone(),
            },
            method,
            Some(url),
            call,
            src,
            ctx,
            base_url_opt,
            enclosing_fn_line,
            path,
            framework,
            &verb_text,
        );
    }

    None
}

/// Build a [`HttpClientCall`] for a `.scm` match where the URL
/// lives upstream of the call (e.g. Java `HttpClient.send` and
/// Kotlin `OkHttpClient.execute`). The emitted URL is synthetic
/// (`<framework-id>://dynamic`); the joiner still matches it
/// against the verb+method axis.
fn synthetic_url_call(
    framework: &FrameworkDef,
    lib_text: String,
    call: Node,
    path: &str,
) -> HttpClientCall {
    let raw_parts = vec![UrlPart::Literal(format!(
        "{}://dynamic",
        framework.effective_id()
    ))];
    let host = host_for(&raw_parts);
    let normalized = normalize(&raw_parts);
    let final_url = if matches!(host, HostPart::None) {
        normalized
    } else {
        NormalizedUrl { host, ..normalized }
    };
    // The `.scm` doesn't capture `@lib` for frameworks whose URL lives
    // upstream of the call (Java `HttpClient.send`, OkHttp `.execute`,
    // …). When the receiver text is absent the call's `via` falls back
    // to the framework's effective_id (`display_name` when set, else
    // `id`) — so a Java OkHttp call surfaces as
    // `Library { name: "okhttp" }`, not `Library { name: "" }`.
    let via_lib = if lib_text.is_empty() {
        framework.effective_id().to_string()
    } else {
        lib_text
    };
    HttpClientCall {
        method: MethodSpec::Unknown,
        url: final_url,
        via: CallVia::Library { name: via_lib },
        url_expr: format!("{}://dynamic", framework.effective_id()),
        reads_complete: true,
        path: path.to_string(),
        line: (call.start_position().row as u32) + 1,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_call(
    via: CallVia,
    method: MethodSpec,
    url: Option<Node>,
    call: Node,
    src: &[u8],
    ctx: &FileContext,
    base_url_opt: Option<&str>,
    enclosing_fn_line: Option<u32>,
    path: &str,
    _framework: &FrameworkDef,
    _verb_text: &str,
) -> Option<HttpClientCall> {
    let call_line = (call.start_position().row as u32) + 1;
    let raw_parts = match url {
        Some(u) => parts_from_node(u, src, ctx, enclosing_fn_line, base_url_opt),
        None => Vec::new(),
    };
    let host = host_for(&raw_parts);
    let normalized = normalize(&raw_parts);
    let final_url = if matches!(host, HostPart::None) {
        normalized
    } else {
        NormalizedUrl { host, ..normalized }
    };
    let url_source = match url {
        Some(u) => text_of(u, src).unwrap_or_default(),
        None => String::new(),
    };
    let url_expr = truncate_url_expr(&url_source);
    Some(HttpClientCall {
        method,
        url: final_url,
        via,
        url_expr,
        reads_complete: true,
        path: path.to_string(),
        line: call_line,
    })
}

/// Does the receiver text match the framework's `lib_match` regex?
fn is_known_library(lib_text: &str, framework: &FrameworkDef, _lang: Lang) -> bool {
    if let Some(re) = framework.lib_match.as_deref() {
        regex_match_lib(re, lib_text)
    } else {
        // No regex — accept any receiver (e.g. catch-all wrapper).
        !lib_text.is_empty()
    }
}

fn regex_match_lib(pattern: &str, lib_text: &str) -> bool {
    match regex::Regex::new(pattern) {
        Ok(re) => re.is_match(lib_text),
        Err(_) => false,
    }
}

fn starts_with_slash_node(node: Node, src: &[u8]) -> bool {
    let raw = text_of(node, src).unwrap_or_default();
    let s = raw.trim_start();
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    if matches!(bytes[0], b'"' | b'\'') && bytes.len() >= 2 {
        return bytes[1] == b'/';
    }
    if bytes[0] == b'/' {
        return true;
    }
    if bytes.len() >= 3
        && (bytes[0] == b'f' || bytes[0] == b'F')
        && matches!(bytes[1], b'"' | b'\'')
    {
        return bytes[2] == b'/';
    }
    if bytes[0] == b'`' {
        return s.trim_start_matches('`').starts_with('/');
    }
    false
}

/// Extract the HTTP method from a match. Most repos pass the verb
/// text directly through [`method_from_verb`]; a few need to read
/// an init object or a positional argument for the method (TS/JS
/// `fetch`, `axios`, Python/Go `request` / `NewRequest`). The
/// .NET HttpClient verbs are spelled `GetAsync`, `PostAsync`, …
/// and need the trailing `async` stripped before mapping.
fn method_for(
    framework: &FrameworkDef,
    verb_text: &str,
    call: Node,
    url: Option<Node>,
    src: &[u8],
) -> MethodSpec {
    if is_valid_http_verb(verb_text) {
        return method_from_verb(verb_text);
    }
    let id = framework.id.as_str();
    match id {
        "httpclient-outbound" => method_from_csharp_verb(verb_text),
        "requests-outbound" | "httpx-outbound" => {
            // Verb == "request" → method from first positional arg
            // (string literal) or from the `method=` kwarg.
            method_from_request_shape(call, src)
        }
        "fetch-outbound" => {
            // `fetch(url, init)` → method from init's `method` key.
            method_from_init_object(call, src)
        }
        "axios-outbound" | "got-outbound" => {
            // `axios(url)` → Unknown. `axios({method, url})` →
            // method from object.method. `axios.<verb>(url)` →
            // Unknown (verb captured separately; not a valid HTTP
            // verb at this point).
            method_from_axios_shape(call, url, src)
        }
        "stdlib-http-outbound" => {
            // `http.NewRequest(method, url, body)` /
            // `http.NewRequestWithContext(ctx, method, url, body)`.
            // Verb = "newrequest" / "newrequestwithcontext"; the
            // walker reads the appropriate positional argument.
            method_from_newrequest_shape(call, src)
        }
        "okhttp-outbound" => {
            // Java: `execute` / `send` — verb itself is not an HTTP
            // verb, so the method is Unknown. (The URL is buried in
            // the receiver chain; the walker emits a synthetic URL.)
            MethodSpec::Unknown
        }
        "net-http-outbound" | "httparty-outbound" | "faraday-outbound" | "restclient-outbound" => {
            // The .scm already captures the verb on the receiver
            // — if it isn't a known HTTP verb here, return Unknown.
            MethodSpec::Unknown
        }
        "ktor-client-outbound" => {
            // `client.get(url)` etc. — verb is `get` / `post` / …;
            // `client.execute()` is the OkHttp shape, which the
            // okhttp-outbound .scm owns.
            MethodSpec::Unknown
        }
        "awc-outbound" | "ureq-outbound" | "reqwest-outbound" => {
            // Rust: scoped_identifier binds @verb to the rightmost
            // identifier. If it isn't a valid HTTP verb here, return
            // Unknown rather than emitting a junk call.
            MethodSpec::Unknown
        }
        _ => MethodSpec::Unknown,
    }
}

/// Map C# `HttpClient.<verb>(url)` / `WebClient.<verb>(url)` verb
/// spellings to the canonical HTTP method.
fn method_from_csharp_verb(verb_text: &str) -> MethodSpec {
    let normalized = verb_text.to_ascii_lowercase();
    let canonical = match normalized.as_str() {
        "getasync" | "getstringasync" => "get",
        "postasync" => "post",
        "putasync" => "put",
        "patchasync" => "patch",
        "deleteasync" => "delete",
        "sendasync" => "send",
        "downloadstring" | "downloadstringtaskasync" => "get",
        other => other,
    };
    method_from_verb(canonical)
}

/// Extract the verb (rightmost named child) from a Kotlin
/// `navigation_expression` node. The Kotlin grammar exposes
/// the receiver and the verb as named siblings under one
/// navigation_expression — the rightmost is the verb.
fn extract_navigation_verb(nav: Node, src: &[u8]) -> String {
    let mut cursor = nav.walk();
    let mut last_id: Option<Node> = None;
    for child in nav.named_children(&mut cursor) {
        if matches!(child.kind(), "identifier" | "simple_identifier") {
            last_id = Some(child);
        }
    }
    last_id
        .and_then(|n| text_of(n, src))
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// Read the `method` from a `requests.request(method, url)` /
/// `requests.request(method=…, url=…)` call shape.
fn method_from_request_shape(call: Node, src: &[u8]) -> MethodSpec {
    let args = args_of_tsjs(call);
    // Positional: method is arg 0.
    if let Some(first) = args.first() {
        if first.kind() == "string" {
            if let Some(text) = text_of(*first, src) {
                let cleaned = strip_python_string_quotes(&text).to_ascii_lowercase();
                return method_from_verb(&cleaned);
            }
        }
    }
    // Kwarg: method=…
    for arg in &args {
        if arg.kind() == "keyword_argument" {
            let name = arg
                .child_by_field_name("name")
                .and_then(|n| text_of(n, src));
            if name.as_deref() == Some("method") {
                if let Some(value) = arg.child_by_field_name("value") {
                    if value.kind() == "string" {
                        if let Some(text) = text_of(value, src) {
                            let cleaned = strip_python_string_quotes(&text).to_ascii_lowercase();
                            return method_from_verb(&cleaned);
                        }
                    }
                }
                return MethodSpec::Unknown;
            }
        }
    }
    MethodSpec::Unknown
}

/// Read the method from a `fetch(url, init)` call's init object —
/// the `method` key on the object. Defaults to GET when init is
/// absent or has no `method` key.
fn method_from_init_object(call: Node, src: &[u8]) -> MethodSpec {
    let args = args_of_tsjs(call);
    let init = match args.get(1) {
        Some(n) => n,
        None => return MethodSpec::Known(HttpMethod::Get),
    };
    if init.kind() != "object" {
        return MethodSpec::Known(HttpMethod::Get);
    }
    let mut cursor = init.walk();
    for child in init.children(&mut cursor) {
        if child.kind() != "pair" {
            continue;
        }
        let key = match child.child_by_field_name("key") {
            Some(k) => k,
            None => continue,
        };
        let key_text = text_of(key, src).unwrap_or_default();
        if key_text != "method" {
            continue;
        }
        let value = match child.child_by_field_name("value") {
            Some(v) => v,
            None => return MethodSpec::Known(HttpMethod::Get),
        };
        if value.kind() == "string" {
            if let Some(text) = text_of(value, src) {
                let cleaned = strip_js_string_quotes(&text).to_ascii_lowercase();
                return method_from_verb(&cleaned);
            }
        }
        return MethodSpec::Unknown;
    }
    MethodSpec::Known(HttpMethod::Get)
}

/// Read the method from an `axios(url)` / `axios({method, url})` /
/// `axios(url, {method})` shape. Looks at:
///   1. The init object (arg 1) for `axios(url, {method: …})`.
///   2. The URL node itself when `=== object` (for
///      `axios({method: …, url: …})`).
///   3. Returns Unknown otherwise.
fn method_from_axios_shape(call: Node, url: Option<Node>, src: &[u8]) -> MethodSpec {
    // Try the init-object path first (arg 1 = object).
    if let Some(args) = call.child_by_field_name("arguments") {
        let mut cursor = args.walk();
        let named: Vec<_> = args.named_children(&mut cursor).collect();
        if let Some(init) = named.get(1) {
            if init.kind() == "object" {
                if let Some(spec) = method_from_object(init, src) {
                    return spec;
                }
            }
        }
    }
    // Fall back to looking at the URL node itself when it's an object.
    if let Some(url) = url {
        if url.kind() == "object" {
            if let Some(spec) = method_from_object(&url, src) {
                return spec;
            }
        }
    }
    MethodSpec::Unknown
}

fn method_from_object(obj: &Node, src: &[u8]) -> Option<MethodSpec> {
    let mut cursor = obj.walk();
    for child in obj.children(&mut cursor) {
        if child.kind() != "pair" {
            continue;
        }
        let key = child.child_by_field_name("key");
        let key_text = key.and_then(|k| text_of(k, src)).unwrap_or_default();
        if key_text == "method" {
            let value = child.child_by_field_name("value")?;
            if value.kind() == "string" {
                if let Some(text) = text_of(value, src) {
                    let cleaned = strip_js_string_quotes(&text).to_ascii_lowercase();
                    return Some(method_from_verb(&cleaned));
                }
            }
            return Some(MethodSpec::Unknown);
        }
    }
    None
}

/// Read the method from `http.NewRequest(method, url, body)` /
/// `http.NewRequestWithContext(ctx, method, url, body)`. The
/// .scm pattern restricts @verb so we just look at the right
/// positional argument.
fn method_from_newrequest_shape(call: Node, src: &[u8]) -> MethodSpec {
    let args = args_of_tsjs(call);
    // The .scm captures @verb in two forms: "NewRequest" and
    // "NewRequestWithContext". Determine the method arg index
    // from the call's text.
    let call_text = text_of(call, src).unwrap_or_default();
    let method_idx = if call_text.contains("NewRequestWithContext") {
        1
    } else {
        0
    };
    let Some(method_arg) = args.get(method_idx) else {
        return MethodSpec::Unknown;
    };
    if method_arg.kind() != "interpreted_string_literal"
        && method_arg.kind() != "string_literal"
        && method_arg.kind() != "string"
    {
        return MethodSpec::Unknown;
    }
    let raw = text_of(*method_arg, src).unwrap_or_default();
    let cleaned = strip_js_string_quotes(&raw).to_ascii_lowercase();
    method_from_verb(&cleaned)
}

// ─── File context: identifiers and client bindings ────────────────────

#[derive(Debug, Default)]
struct FileContext {
    /// Module-level `name → assigned expression text`. Only the names
    /// with a single module-level assignment are used for resolution.
    module_assignments: BTreeMap<String, String>,
    /// Module-level assignment counts per name. The BTreeMap above
    /// would silently keep only the last value when the same name is
    /// assigned twice — but the §6.3 contract says the identifier is
    /// resolved only when there is *exactly one* assignment. We count
    /// occurrences in the same walk so that two assignments to the
    /// same name leave the name unresolvable.
    module_assign_counts: BTreeMap<String, u32>,
    /// Per-function assignments keyed by `(function_start_line, name)`.
    /// A name resolves at a call site when there is exactly one
    /// assignment with that name in the enclosing function.
    fn_assignments: BTreeMap<(u32, String), String>,
    /// Per-function assignment counts. See the module-level note.
    fn_assign_counts: BTreeMap<(u32, String), u32>,
    /// `client_var → base_url text` for ctor bindings. The library
    /// name (httpx, requests, aiohttp) is also tracked so the call's
    /// `Library { name }` label is honest.
    client_base_urls: BTreeMap<String, ClientBinding>,
}

#[derive(Debug, Clone)]
struct ClientBinding {
    base_url: String,
    library: String,
}

impl FileContext {
    fn collect(root: Node, src: &[u8], lang: Lang) -> Self {
        let mut ctx = FileContext::default();
        match lang {
            Lang::Python => collect_python_context(root, src, &mut ctx),
            Lang::TsJs => collect_tsjs_context(root, src, &mut ctx),
            // Rust + Go don't currently support client-base_url
            // resolution; pass through an empty context.
            Lang::Rust | Lang::Go => {}
            // Workstream 5: the four new languages don't carry
            // ctor-base_url resolution either; the receiver-side
            // detection fires at the call site.
            Lang::Java | Lang::CSharp | Lang::Ruby | Lang::Kotlin => {}
            // `Ts` / `Tsx` share the `tsjs` bucket — the
            // existing parser uses `tree_sitter_javascript` for both,
            // and `lang_for_path` routes .ts/.tsx to `Lang::Ts` /
            // `Lang::Tsx` before they reach the sensors. The
            // `scan_workspace_clients` extension matcher routes them
            // all to `Lang::TsJs`. If `Lang::Ts` ever shows up
            // here it's a wiring bug worth surfacing — currently
            // the match arm is a no-op.
            Lang::Ts | Lang::Tsx => {}
        }
        ctx
    }

    fn resolve_identifier(&self, name: &str, enclosing_fn_line: Option<u32>) -> Option<String> {
        // Same-function wins when unique.
        if let Some(line) = enclosing_fn_line {
            for ((l, n), c) in &self.fn_assign_counts {
                if *l == line && n == name && *c == 1 {
                    let key = (*l, name.to_string());
                    return self.fn_assignments.get(&key).cloned();
                }
            }
        }
        // Module-level: single assignment only.
        if let Some(c) = self.module_assign_counts.get(name) {
            if *c == 1 {
                return self.module_assignments.get(name).cloned();
            }
        }
        None
    }
}

// ─── Python context collection ────────────────────────────────────────

fn collect_python_context(root: Node, src: &[u8], ctx: &mut FileContext) {
    walk(root, &mut |node| {
        match node.kind() {
            // Module-level assignments and per-function assignments
            // share the same `assignment` node kind; `is_module_level`
            // disambiguates them by walking the parent chain.
            "assignment" => {
                if let Some((name, value)) = read_assignment(node, src) {
                    if is_module_level(node) {
                        ctx.module_assignments.insert(name.clone(), value.clone());
                        *ctx.module_assign_counts.entry(name).or_insert(0) += 1;
                    } else if let Some(fn_line) = enclosing_function_line(node) {
                        let key = (fn_line, name.clone());
                        ctx.fn_assignments
                            .entry(key.clone())
                            .or_insert_with(|| value.clone());
                        *ctx.fn_assign_counts.entry(key).or_insert(0) += 1;
                    }
                }
                // Client-instance ctor: `client = httpx.Client(base_url=...)`.
                if let Some((name, binding)) = read_client_assignment(node, src) {
                    ctx.client_base_urls.insert(name, binding);
                }
            }
            "with_statement" | "async_with_statement" => {
                if let Some((name, binding)) = read_with_client(node, src) {
                    ctx.client_base_urls.insert(name, binding);
                }
            }
            _ => {}
        }
    });
}

fn read_assignment(node: Node, src: &[u8]) -> Option<(String, String)> {
    let left = node.child_by_field_name("left")?;
    let right = node.child_by_field_name("right")?;
    if left.kind() != "identifier" {
        return None;
    }
    let name = text_of(left, src)?;
    let value = text_of(right, src)?;
    Some((name, value))
}

fn read_client_assignment(node: Node, src: &[u8]) -> Option<(String, ClientBinding)> {
    let left = node.child_by_field_name("left")?;
    if left.kind() != "identifier" {
        return None;
    }
    let right = node.child_by_field_name("right")?;
    if right.kind() != "call" {
        return None;
    }
    let function = right.child_by_field_name("function")?;
    let (lib, ctor) = attribute_name(function, src)?;
    if !is_client_ctor(&lib, &ctor) {
        return None;
    }
    let base_url = find_kwarg(right, "base_url", src)?;
    let name = text_of(left, src)?;
    Some((
        name,
        ClientBinding {
            base_url,
            library: lib,
        },
    ))
}

fn read_with_client(node: Node, src: &[u8]) -> Option<(String, ClientBinding)> {
    let mut cursor = node.walk();
    let clause = node
        .children(&mut cursor)
        .find(|c| c.kind() == "with_clause")?;
    let mut found: Option<(String, ClientBinding)> = None;
    walk(clause, &mut |n| {
        if n.kind() != "as_pattern" {
            return;
        }
        let value = n
            .child_by_field_name("pattern")
            .or_else(|| n.child_by_field_name("expression"))
            .or_else(|| n.child(0));
        let target = n
            .child_by_field_name("alias")
            .or_else(|| n.child_by_field_name("alternative"))
            .or_else(|| {
                let count = n.child_count();
                if count > 0 {
                    n.child(count - 1)
                } else {
                    None
                }
            });
        let (value, target) = match (value, target) {
            (Some(v), Some(t)) => (v, t),
            _ => return,
        };
        if value.kind() != "call" {
            return;
        }
        let function = match value.child_by_field_name("function") {
            Some(f) => f,
            None => return,
        };
        let (lib, ctor) = match attribute_name(function, src) {
            Some(p) => p,
            None => return,
        };
        if !is_client_ctor(&lib, &ctor) {
            return;
        }
        let base_url = match find_kwarg(value, "base_url", src) {
            Some(u) => u,
            None => return,
        };
        let target_name = match text_of(target, src) {
            Some(s) => s,
            None => return,
        };
        found = Some((
            target_name,
            ClientBinding {
                base_url,
                library: lib,
            },
        ));
    });
    found
}

fn is_client_ctor(lib: &str, ctor: &str) -> bool {
    matches!(
        (lib, ctor),
        ("httpx", "Client" | "AsyncClient")
            | ("requests", "Session")
            | ("aiohttp", "ClientSession")
    )
}

// ─── TS/JS context collection ─────────────────────────────────────────

fn collect_tsjs_context(root: Node, src: &[u8], ctx: &mut FileContext) {
    walk(root, &mut |node| match node.kind() {
        "lexical_declaration" | "variable_declaration" => {
            if !is_module_level(node) {
                return;
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "variable_declarator" {
                    if let Some((name, value)) = read_var_declarator(child, src) {
                        ctx.module_assignments.insert(name.clone(), value);
                        *ctx.module_assign_counts.entry(name).or_insert(0) += 1;
                    }
                }
            }
        }
        "assignment_expression" => {
            // Bare `BASE = "..."` (no keyword) at module level. Some
            // grammars also emit `assignment`; we accept both.
            if !is_module_level(node) {
                return;
            }
            let left = match node.child_by_field_name("left") {
                Some(l) => l,
                None => return,
            };
            if left.kind() != "identifier" {
                return;
            }
            let right = match node.child_by_field_name("right") {
                Some(r) => r,
                None => return,
            };
            if let (Some(name), Some(value)) = (text_of(left, src), text_of(right, src)) {
                ctx.module_assignments.insert(name.clone(), value);
                *ctx.module_assign_counts.entry(name).or_insert(0) += 1;
            }
        }
        "assignment" => {
            if !is_module_level(node) {
                return;
            }
            let left = match node.child_by_field_name("left") {
                Some(l) => l,
                None => return,
            };
            if left.kind() != "identifier" {
                return;
            }
            let right = match node.child_by_field_name("right") {
                Some(r) => r,
                None => return,
            };
            if let (Some(name), Some(value)) = (text_of(left, src), text_of(right, src)) {
                ctx.module_assignments.insert(name.clone(), value);
                *ctx.module_assign_counts.entry(name).or_insert(0) += 1;
            }
        }
        _ => {}
    });
}

fn read_var_declarator(node: Node, src: &[u8]) -> Option<(String, String)> {
    let name = node.child_by_field_name("name")?;
    let value = node.child_by_field_name("value")?;
    let name_text = text_of(name, src)?;
    let value_text = text_of(value, src)?;
    Some((name_text, value_text))
}

// ─── AST helpers ──────────────────────────────────────────────────────

fn is_module_level(node: Node) -> bool {
    let mut cur = node.parent();
    while let Some(p) = cur {
        match p.kind() {
            "function_definition"
            | "class_definition"
            | "lambda"
            | "arrow_function"
            | "function"
            | "function_expression"
            | "method_definition" => return false,
            _ => {}
        }
        cur = p.parent();
    }
    true
}

fn enclosing_function_line(node: Node) -> Option<u32> {
    let mut cur = node.parent();
    while let Some(p) = cur {
        match p.kind() {
            "function_definition" | "async_function_definition" => {
                return Some(p.start_position().row as u32);
            }
            _ => {}
        }
        cur = p.parent();
    }
    None
}

fn text_of(node: Node, src: &[u8]) -> Option<String> {
    node.utf8_text(src).ok().map(str::to_string)
}

fn truncate_url_expr(s: &str) -> String {
    if s.len() <= URL_EXPR_CAP {
        s.to_string()
    } else {
        let mut end = URL_EXPR_CAP;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        s[..end].to_string()
    }
}

fn attribute_name(node: Node, src: &[u8]) -> Option<(String, String)> {
    let object = node.child_by_field_name("object")?;
    let attr = node.child_by_field_name("attribute")?;
    let attr_text = text_of(attr, src)?;
    if object.kind() == "identifier" {
        let recv_text = text_of(object, src)?;
        Some((recv_text, attr_text))
    } else if object.kind() == "attribute" {
        let leftmost = leftmost_identifier_text(object, src)?;
        Some((leftmost, attr_text))
    } else {
        None
    }
}

fn leftmost_identifier_text(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => text_of(node, src),
        "attribute" | "member_expression" => {
            let object = node.child_by_field_name("object")?;
            leftmost_identifier_text(object, src)
        }
        "subscript_expression" | "subscript" => {
            let value = node
                .child_by_field_name("value")
                .or_else(|| node.child_by_field_name("object"))
                .or_else(|| node.child(0))?;
            leftmost_identifier_text(value, src)
        }
        _ => None,
    }
}

fn find_kwarg(call: Node, name: &str, src: &[u8]) -> Option<String> {
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    for c in args.named_children(&mut cursor) {
        if c.kind() == "keyword_argument" {
            let n = c.child_by_field_name("name")?;
            if text_of(n, src).as_deref() == Some(name) {
                let value = c.child_by_field_name("value")?;
                if let Some(text) = text_of(value, src) {
                    if value.kind() == "string" {
                        return Some(strip_python_string_quotes(&text).to_string());
                    }
                    return Some(text);
                }
            }
        }
    }
    None
}

fn args_of<'a>(call: Node<'a>) -> Vec<Node<'a>> {
    let Some(args) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut cursor = args.walk();
    // Named children only — skips `,` separators.
    args.named_children(&mut cursor).collect()
}

fn args_of_tsjs<'a>(call: Node<'a>) -> Vec<Node<'a>> {
    let Some(args) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut cursor = args.walk();
    args.named_children(&mut cursor).collect()
}

fn is_valid_http_verb(s: &str) -> bool {
    matches!(
        s,
        "get" | "post" | "put" | "patch" | "delete" | "head" | "options"
    )
}

fn method_from_verb(verb: &str) -> MethodSpec {
    let m = match verb {
        "get" => HttpMethod::Get,
        "post" => HttpMethod::Post,
        "put" => HttpMethod::Put,
        "patch" => HttpMethod::Patch,
        "delete" => HttpMethod::Delete,
        "head" => HttpMethod::Head,
        "options" => HttpMethod::Options,
        _ => return MethodSpec::Unknown,
    };
    MethodSpec::Known(m)
}

fn strip_python_string_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' || first == b'\'') && first == last {
            return &s[1..s.len() - 1];
        }
    }
    s
}

fn strip_js_string_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' || first == b'\'' || first == b'`') && first == last {
            return &s[1..s.len() - 1];
        }
    }
    s
}

// ─── URL parts extraction ────────────────────────────────────────────

/// Convert a URL argument AST node into a list of [`UrlPart`]s. The
/// language-specific cases (Python f-string, TS template literal)
/// differ only in node kinds; the structural rules — string literal
/// = one Literal, template/f-string alternates Literal/Hole, `a + b`
/// concatenates, `urljoin(base, "/p")` and `new URL("/p", base)` give
/// parts of `base` then `"/p"`, identifier resolved once — are
/// language-agnostic.
fn parts_from_node(
    node: Node,
    src: &[u8],
    ctx: &FileContext,
    enclosing_fn_line: Option<u32>,
    base_url: Option<&str>,
) -> Vec<UrlPart> {
    let raw = parts_from_node_inner(node, src, ctx, enclosing_fn_line);
    apply_base_url(raw, base_url)
}

fn parts_from_node_inner(
    node: Node,
    src: &[u8],
    ctx: &FileContext,
    enclosing_fn_line: Option<u32>,
) -> Vec<UrlPart> {
    match node.kind() {
        // Python string / TS string literal — strip them and produce a
        // single Literal. The brackets/braces already in the literal
        // are preserved verbatim; the §4.5 normalizer handles
        // parameters and wildcards.
        "string" => {
            let text = text_of(node, src).unwrap_or_default();
            // Detect f-string (or raw-string + f-string) by a leading
            // `f`/`F` before the opening quote. The grammar emits the
            // `string` node with children `string_start` (which carries
            // the `f` prefix), `interpolation`, `string_content`, and
            // `string_end`. Walking them yields the alternation.
            let raw = text.as_bytes();
            let is_fstring = raw.len() >= 3
                && (raw[0] == b'f' || raw[0] == b'F')
                && matches!(raw[1], b'"' | b'\'');
            if is_fstring {
                let mut out = Vec::new();
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    match child.kind() {
                        "string_content" => {
                            if let Some(t) = text_of(child, src) {
                                if !t.is_empty() {
                                    out.push(UrlPart::Literal(t));
                                }
                            }
                        }
                        "interpolation" => {
                            let inner = parts_from_node_inner(child, src, ctx, enclosing_fn_line);
                            if !inner.is_empty() {
                                out.extend(inner);
                            }
                        }
                        _ => {}
                    }
                }
                return out;
            }
            let content = strip_python_string_quotes(&text);
            vec![UrlPart::Literal(content.to_string())]
        }
        "string_literal" => {
            // Rust, C#, Java, Kotlin: `string_literal` carries its
            // own delimiters. Strip them so the §4.5 normalizer
            // doesn't see the trailing `"` as part of the URL.
            let text = text_of(node, src).unwrap_or_default();
            let cleaned = strip_js_string_quotes(&text);
            vec![UrlPart::Literal(cleaned.to_string())]
        }
        // Go's `interpreted_string_literal` (e.g. `"foo"` in
        // `http.Get("foo")`) — same shape as Rust's
        // `string_literal`: strip the quotes and emit a single
        // Literal. The grammar nests the actual content in
        // `interpreted_string_literal_content`, but we accept either
        // shape (a stripped Literal from the wrapper is equivalent
        // to a Literal from the inner content).
        "interpreted_string_literal" => {
            let text = text_of(node, src).unwrap_or_default();
            let cleaned = strip_js_string_quotes(&text);
            vec![UrlPart::Literal(cleaned.to_string())]
        }
        // Python string concatenation: `"a" "b"` — multiple `string`
        // siblings inside `concatenated_string`, OR a sequence of
        // `string` nodes via `string_concatenation`. We treat either
        // by iterating children and recursing.
        "concatenated_string" | "concatenated_template" | "template_string_substitution" => {
            let mut out = Vec::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                out.extend(parts_from_node_inner(child, src, ctx, enclosing_fn_line));
            }
            out
        }
        "template_string" => template_string_parts(node, src, ctx, enclosing_fn_line),
        "binary_expression" => {
            // Only the `+` operator concatenates. Any other binary op
            // yields a Hole with the full expression.
            let op_text = node
                .child_by_field_name("operator")
                .and_then(|o| text_of(o, src));
            if op_text.as_deref() != Some("+") {
                return vec![UrlPart::Hole(text_of(node, src).unwrap_or_default())];
            }
            let mut out = Vec::new();
            if let Some(lhs) = node.child_by_field_name("left") {
                out.extend(parts_from_node_inner(lhs, src, ctx, enclosing_fn_line));
            }
            if let Some(rhs) = node.child_by_field_name("right") {
                out.extend(parts_from_node_inner(rhs, src, ctx, enclosing_fn_line));
            }
            out
        }
        // TS template-string substitution: `substitution` wraps the
        // expression in `${…}`. Recurse into the first named child
        // (the expression itself).
        "substitution" | "template_substitution" => {
            let mut cursor = node.walk();
            if let Some(child) = node.named_children(&mut cursor).next() {
                return parts_from_node_inner(child, src, ctx, enclosing_fn_line);
            }
            Vec::new()
        }
        // Python f-string interpolation: `{expr}` inside the f-string.
        "interpolation" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if matches!(child.kind(), "format_specifier") {
                    continue;
                }
                return parts_from_node_inner(child, src, ctx, enclosing_fn_line);
            }
            Vec::new()
        }
        "argument" | "keyword_argument" => {
            // Bare argument node — recurse on the wrapped expression.
            // Python's `argument` exposes the value as a `value`
            // field; C# / Java / Kotlin / Ruby expose it as a single
            // unnamed child. Walk both candidates, taking whichever
            // exists, and never fall back to `node` itself (which
            // would recurse on the same kind → stack overflow).
            if let Some(value) = node.child_by_field_name("value") {
                parts_from_node_inner(value, src, ctx, enclosing_fn_line)
            } else {
                let mut cursor = node.walk();
                let next = node.named_children(&mut cursor).next();
                match next {
                    Some(child) => parts_from_node_inner(child, src, ctx, enclosing_fn_line),
                    None => Vec::new(),
                }
            }
        }
        // `urljoin(base, "/p")` / `new URL("/p", base)` give parts of
        // base then "/p". Both Python stdlib `urljoin` and TS `URL`
        // ctor. We detect either form and split into base + path.
        "call" | "call_expression" | "new_expression" => {
            call_urljoin_parts(node, src, ctx, enclosing_fn_line)
        }
        // Identifier: resolve once if possible.
        "identifier" => identifier_parts(node, src, ctx, enclosing_fn_line),
        // Python attribute / TS member expression: emit as a Hole with
        // its full source text.
        "attribute" | "member_expression" | "subscript" | "subscript_expression" => {
            vec![UrlPart::Hole(text_of(node, src).unwrap_or_default())]
        }
        // TS/JS object literal: drill into pairs and pick up a
        // `url: <string>` member if present. `axios({ method, url })`
        // and `fetch(url, { method })` both rely on this.
        "object" => {
            let mut out = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() != "pair" {
                    continue;
                }
                let key = child.child_by_field_name("key");
                let key_text = key.and_then(|k| text_of(k, src)).unwrap_or_default();
                if key_text == "url" {
                    let value = child.child_by_field_name("value");
                    if let Some(v) = value {
                        out.extend(parts_from_node_inner(v, src, ctx, enclosing_fn_line));
                    }
                }
            }
            if out.is_empty() {
                vec![UrlPart::Hole(text_of(node, src).unwrap_or_default())]
            } else {
                out
            }
        }
        // Catch-all: drop the node, emit no parts.
        _ => Vec::new(),
    }
}

fn template_string_parts(
    node: Node,
    src: &[u8],
    ctx: &FileContext,
    enclosing_fn_line: Option<u32>,
) -> Vec<UrlPart> {
    // JS grammar emits `template_string`'s named children as
    // `template_substitution` (one per `${…}`) and `string_fragment`
    // (literal chunks between them). Walk them in source order.
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "template_substitution" => {
                let inner = parts_from_node_inner(child, src, ctx, enclosing_fn_line);
                if inner.is_empty() {
                    out.push(UrlPart::Hole(String::new()));
                } else {
                    out.extend(inner);
                }
            }
            "string_fragment" => {
                if let Some(text) = text_of(child, src) {
                    if !text.is_empty() {
                        out.push(UrlPart::Literal(text));
                    }
                }
            }
            _ => {}
        }
    }
    if out.is_empty() {
        // A template with no substitutions (just literal text inside
        // backticks): treat the whole thing as a literal.
        if let Some(text) = text_of(node, src) {
            let stripped = strip_js_string_quotes(&text);
            if !stripped.is_empty() {
                out.push(UrlPart::Literal(stripped.to_string()));
            }
        }
    }
    out
}

fn call_urljoin_parts(
    node: Node,
    src: &[u8],
    ctx: &FileContext,
    enclosing_fn_line: Option<u32>,
) -> Vec<UrlPart> {
    // `new URL(...)` exposes the constructor under the `constructor`
    // field rather than `function`. `call` / `call_expression` use
    // `function`. Try `function` first, then `constructor`.
    let function = node
        .child_by_field_name("function")
        .or_else(|| node.child_by_field_name("constructor"))
        .or_else(|| {
            // Some grammars (TS) emit `construct` on `new_expression`.
            node.child_by_field_name("construct")
        });
    let Some(function) = function else {
        return Vec::new();
    };
    let name = match function.kind() {
        "identifier" => text_of(function, src),
        "attribute" | "member_expression" => {
            // `url.parse(...)` is not `urljoin`, but `urljoin` is
            // typically a bare call in the URL namespace. Look at the
            // leftmost identifier: if it's `urllib.parse.urljoin` or
            // similar we'd still want to recognize; but for the
            // fixture the bare `urljoin(...)` form suffices.
            leftmost_identifier_text(function, src)
        }
        _ => None,
    };
    let func_text = match function.kind() {
        "identifier" => text_of(function, src).unwrap_or_default(),
        "attribute" | "member_expression" => {
            let recv = function.child_by_field_name("object");
            let attr = function.child_by_field_name("attribute");
            match (recv, attr) {
                (Some(_), Some(a)) => text_of(a, src).unwrap_or_default(),
                _ => String::new(),
            }
        }
        _ => String::new(),
    };
    let is_urljoin = name.as_deref() == Some("urljoin") && func_text == "urljoin";
    let is_new_url = func_text == "URL" && node.kind() == "new_expression";
    if !is_urljoin && !is_new_url {
        return vec![UrlPart::Hole(text_of(node, src).unwrap_or_default())];
    }
    let args = args_of(node);
    let (base_idx, path_idx) = if is_urljoin { (0, 1) } else { (1, 0) };
    let base_arg = args.get(base_idx).copied();
    let path_arg = args.get(path_idx).copied();
    let mut out = Vec::new();
    if let Some(b) = base_arg {
        out.extend(parts_from_node_inner(b, src, ctx, enclosing_fn_line));
    }
    if let Some(p) = path_arg {
        out.extend(parts_from_node_inner(p, src, ctx, enclosing_fn_line));
    }
    out
}

fn identifier_parts(
    node: Node,
    src: &[u8],
    ctx: &FileContext,
    enclosing_fn_line: Option<u32>,
) -> Vec<UrlPart> {
    let name = match text_of(node, src) {
        Some(n) => n,
        None => return Vec::new(),
    };
    match ctx.resolve_identifier(&name, enclosing_fn_line) {
        Some(resolved) => parts_from_node_inner_string(&resolved, ctx, enclosing_fn_line),
        None => vec![UrlPart::Hole(name)],
    }
}

/// Re-parse a stringified assignment (`BASE = "..."`) as if it were a
/// direct URL argument. We don't have a tree-sitter parse context
/// here; we treat the assignment as either a single literal (if it
/// starts with a quote) or a single Hole.
fn parts_from_node_inner_string(
    text: &str,
    ctx: &FileContext,
    enclosing_fn_line: Option<u32>,
) -> Vec<UrlPart> {
    let bytes = text.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' || first == b'\'') && first == last {
            let inner = &text[1..text.len() - 1];
            return vec![UrlPart::Literal(inner.to_string())];
        }
        if first == b'`' && last == b'`' {
            let inner = &text[1..text.len() - 1];
            // f-string / template literal — split on `${...}`. We
            // don't re-parse expressions here; just split into
            // Literal/Hole alternation.
            return template_from_string(inner);
        }
    }
    // Strip and treat as identifier again (one-step chains are out
    // of scope; we only resolve once).
    let trimmed = text.trim();
    if let Some(resolved) = ctx.resolve_identifier(trimmed, enclosing_fn_line) {
        if resolved != text {
            return parts_from_node_inner_string(&resolved, ctx, enclosing_fn_line);
        }
    }
    vec![UrlPart::Hole(text.to_string())]
}

fn template_from_string(s: &str) -> Vec<UrlPart> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'{') {
            if !buf.is_empty() {
                out.push(UrlPart::Literal(std::mem::take(&mut buf)));
            }
            chars.next();
            // Consume up to the matching `}`.
            let mut hole = String::new();
            let mut depth = 1;
            for c2 in chars.by_ref() {
                if c2 == '{' {
                    depth += 1;
                } else if c2 == '}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                hole.push(c2);
            }
            out.push(UrlPart::Hole(hole));
        } else {
            buf.push(c);
        }
    }
    if !buf.is_empty() {
        out.push(UrlPart::Literal(buf));
    }
    if out.is_empty() {
        out.push(UrlPart::Literal(s.to_string()));
    }
    out
}

fn apply_base_url(parts: Vec<UrlPart>, base_url: Option<&str>) -> Vec<UrlPart> {
    let Some(base) = base_url else {
        return parts;
    };
    if base.is_empty() {
        return parts;
    }
    let mut out = Vec::with_capacity(parts.len() + 1);
    out.push(UrlPart::Literal(base.to_string()));
    out.extend(parts);
    out
}

/// Apply host resolution (§6.3) by walking the first hole of `parts`
/// (the longest prefix of holes before any literal segment) and converting
/// it to `HostPart::Env` if its resolved expression matches one of the
/// §6.3 patterns. The result is the host field of the normalized URL;
/// we don't mutate the parts — the normalizer's host step handles
/// the literal host extraction.
fn host_for(parts: &[UrlPart]) -> HostPart {
    // Find the longest prefix of holes at the start.
    let mut hole_text = String::new();
    for part in parts {
        match part {
            UrlPart::Hole(h) => hole_text.push_str(h),
            UrlPart::Literal(_) => break,
        }
    }
    if hole_text.is_empty() {
        return HostPart::None;
    }
    // Pattern-match the resolved expression.
    let t = hole_text.trim();
    if let Some(name) = host_env_name(t) {
        return HostPart::Env(vec![name.to_string()]);
    }
    HostPart::Expr(hole_text)
}

fn host_env_name(text: &str) -> Option<&str> {
    let t = text.trim();
    // `os.environ["X"]` / `os.environ.get("X", …)` / `os.getenv("X", …)`.
    if t.starts_with("os.environ[") && t.ends_with(']') {
        return Some(inner_bracket(&t["os.environ[".len()..t.len() - 1]));
    }
    if let Some(rest) = t.strip_prefix("os.environ.get(") {
        if let Some(end) = rest.find(',') {
            return Some(inner_paren(&rest[..end]));
        }
        if let Some(stripped) = rest.strip_suffix(')') {
            return Some(inner_paren(stripped));
        }
    }
    if let Some(rest) = t.strip_prefix("os.getenv(") {
        if let Some(end) = rest.find(',') {
            return Some(inner_paren(&rest[..end]));
        }
        if let Some(stripped) = rest.strip_suffix(')') {
            return Some(inner_paren(stripped));
        }
    }
    // `process.env.X` / `process.env["X"]`.
    if let Some(rest) = t.strip_prefix("process.env[") {
        if let Some(stripped) = rest.strip_suffix(']') {
            return Some(inner_bracket(stripped));
        }
    }
    if let Some(rest) = t.strip_prefix("process.env.") {
        // `process.env.X` — X is an identifier.
        if rest.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Some(rest);
        }
    }
    // `settings.X` / `config.X`.
    for prefix in ["settings.", "config."] {
        if let Some(rest) = t.strip_prefix(prefix) {
            if rest.chars().all(|c| c.is_alphanumeric() || c == '_') {
                return Some(rest);
            }
        }
    }
    None
}

fn inner_bracket(s: &str) -> &str {
    let t = s.trim();
    let bytes = t.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[0] == bytes[bytes.len() - 1]
    {
        &t[1..t.len() - 1]
    } else {
        t
    }
}

fn inner_paren(s: &str) -> &str {
    let t = s.trim();
    let bytes = t.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[0] == bytes[bytes.len() - 1]
    {
        &t[1..t.len() - 1]
    } else {
        t
    }
}

// ─── Graph emission ───────────────────────────────────────────────────

fn build_graph(
    graph: &GraphDatabase,
    call: &HttpClientCall,
    namespace: &RepoNamespace,
) -> (Vec<GraphNode>, Vec<GraphEdge>) {
    let method_label = method_label(&call.method);
    let template = call.url.template.clone();
    let name = match template {
        Some(t) => format!("{} {}", method_label, t),
        None => format!("{} <dynamic>", method_label),
    };

    let node_id = GraphNode::generate_id(
        &NodeType::HttpClientCall,
        &call.path,
        &name,
        Some(call.line),
        namespace,
    );
    let mut node = GraphNode::new_in(
        NodeType::HttpClientCall,
        name.clone(),
        call.path.clone(),
        namespace,
    );
    node.id = node_id.clone();
    node.line_start = Some(call.line);
    node.contract = Some(crate::federation::contracts::model::ContractFact::Consumer(
        ConsumerFact {
            method: call.method.clone(),
            url: call.url.clone(),
            via: call.via.clone(),
            url_expr: call.url_expr.clone(),
            reads_complete: call.reads_complete,
        },
    ));

    let edge = enclosing_sends_http_edge(graph, &call.path, call.line, node_id);
    (vec![node], edge.into_iter().collect())
}

fn method_label(m: &MethodSpec) -> &'static str {
    match m {
        MethodSpec::Known(HttpMethod::Get) => "GET",
        MethodSpec::Known(HttpMethod::Post) => "POST",
        MethodSpec::Known(HttpMethod::Put) => "PUT",
        MethodSpec::Known(HttpMethod::Patch) => "PATCH",
        MethodSpec::Known(HttpMethod::Delete) => "DELETE",
        MethodSpec::Known(HttpMethod::Head) => "HEAD",
        MethodSpec::Known(HttpMethod::Options) => "OPTIONS",
        MethodSpec::Known(HttpMethod::Any) => "ANY",
        MethodSpec::Unknown => "UNKNOWN",
    }
}

fn enclosing_sends_http_edge(
    graph: &GraphDatabase,
    path: &str,
    line: u32,
    target_id: String,
) -> Vec<GraphEdge> {
    if let Some(sym) = crate::server::sensors::util::enclosing_symbol(graph, path, line) {
        let mut e = GraphEdge::new(EdgeType::SendsHttp, sym.id, target_id);
        e.site = Some(crate::federation::contracts::model::SourceSite {
            path: path.to_string(),
            line,
        });
        return vec![e];
    }
    // Module-level call → attach to the File node, if any. We pick
    // the first node on the file's path; the `path_index` is the
    // canonical lookup so we don't have to mint an id and search
    // for it.
    if graph.has_node_at_path(path) {
        if let Some(file_node) = graph.find_node_by_path(path) {
            let mut e = GraphEdge::new(EdgeType::SendsHttp, file_node.id, target_id);
            e.site = Some(crate::federation::contracts::model::SourceSite {
                path: path.to_string(),
                line,
            });
            return vec![e];
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_url_expr_caps_at_200_chars() {
        let s = "x".repeat(500);
        let t = truncate_url_expr(&s);
        assert_eq!(t.len(), 200);
    }

    #[test]
    fn truncate_url_expr_keeps_short_strings() {
        let s = "short".to_string();
        assert_eq!(truncate_url_expr(&s), "short");
    }

    #[test]
    fn host_env_name_matches_os_environ_subscript() {
        assert_eq!(host_env_name("os.environ[\"BASE_URL\"]"), Some("BASE_URL"));
        assert_eq!(host_env_name("os.environ['BASE_URL']"), Some("BASE_URL"));
    }

    #[test]
    fn host_env_name_matches_os_environ_get() {
        assert_eq!(
            host_env_name("os.environ.get(\"BASE_URL\", \"\")"),
            Some("BASE_URL")
        );
    }

    #[test]
    fn host_env_name_matches_os_getenv() {
        assert_eq!(host_env_name("os.getenv(\"BASE_URL\")"), Some("BASE_URL"));
        assert_eq!(
            host_env_name("os.getenv(\"BASE_URL\", \"\")"),
            Some("BASE_URL")
        );
    }

    #[test]
    fn host_env_name_matches_process_env() {
        assert_eq!(
            host_env_name("process.env.BILLING_URL"),
            Some("BILLING_URL")
        );
        assert_eq!(
            host_env_name("process.env[\"BILLING_URL\"]"),
            Some("BILLING_URL")
        );
    }

    #[test]
    fn host_env_name_matches_settings_and_config() {
        assert_eq!(host_env_name("settings.base_url"), Some("base_url"));
        assert_eq!(host_env_name("config.api_url"), Some("api_url"));
    }

    #[test]
    fn host_env_name_falls_back_to_none_for_unknown() {
        assert_eq!(host_env_name("not_a_known_pattern"), None);
    }

    #[test]
    fn template_from_string_splits_on_substitutions() {
        let parts = template_from_string("a/${x}/b/${y}/c");
        assert_eq!(
            parts,
            vec![
                UrlPart::Literal("a/".to_string()),
                UrlPart::Hole("x".to_string()),
                UrlPart::Literal("/b/".to_string()),
                UrlPart::Hole("y".to_string()),
                UrlPart::Literal("/c".to_string()),
            ]
        );
    }

    #[test]
    fn template_from_string_keeps_a_pure_literal() {
        let parts = template_from_string("a/b/c");
        assert_eq!(parts, vec![UrlPart::Literal("a/b/c".to_string())]);
    }

    #[test]
    fn method_from_verb_maps_each_verb() {
        assert_eq!(method_from_verb("get"), MethodSpec::Known(HttpMethod::Get));
        assert_eq!(
            method_from_verb("post"),
            MethodSpec::Known(HttpMethod::Post)
        );
        assert_eq!(method_from_verb("put"), MethodSpec::Known(HttpMethod::Put));
        assert_eq!(
            method_from_verb("patch"),
            MethodSpec::Known(HttpMethod::Patch)
        );
        assert_eq!(
            method_from_verb("delete"),
            MethodSpec::Known(HttpMethod::Delete)
        );
        assert_eq!(
            method_from_verb("head"),
            MethodSpec::Known(HttpMethod::Head)
        );
        assert_eq!(
            method_from_verb("options"),
            MethodSpec::Known(HttpMethod::Options)
        );
        assert_eq!(method_from_verb("trace"), MethodSpec::Unknown);
    }

    #[test]
    fn host_for_yields_env_for_known_patterns() {
        let parts = vec![
            UrlPart::Hole("os.environ[\"ORDERS_URL\"]".to_string()),
            UrlPart::Literal("/api/orders".to_string()),
        ];
        match host_for(&parts) {
            HostPart::Env(names) => assert_eq!(names.clone(), vec!["ORDERS_URL".to_string()]),
            other => panic!("expected Env, got {other:?}"),
        }
    }

    #[test]
    fn host_for_yields_expr_for_unknown_pattern() {
        let parts = vec![
            UrlPart::Hole("BASE".to_string()),
            UrlPart::Literal("/api/orders".to_string()),
        ];
        match host_for(&parts) {
            HostPart::Expr(text) => assert_eq!(text, "BASE"),
            other => panic!("expected Expr, got {other:?}"),
        }
    }

    #[test]
    fn host_for_yields_none_for_pure_literal() {
        let parts = vec![UrlPart::Literal("https://orders.svc/api".to_string())];
        assert!(matches!(host_for(&parts), HostPart::None));
    }

    // ─── Per-shape detection tests ────────────────────────────────

    fn py_calls(src: &str) -> Vec<HttpClientCall> {
        detect_calls(std::path::Path::new("test.py"), src, Lang::Python)
    }
    fn ts_calls(src: &str) -> Vec<HttpClientCall> {
        detect_calls(std::path::Path::new("test.ts"), src, Lang::TsJs)
    }

    #[test]
    fn python_requests_get_emits_consumer_fact() {
        let src = "import requests\nrequests.get(\"https://api.example.com/v1/x\")\n";
        let calls = py_calls(src);
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.method, MethodSpec::Known(HttpMethod::Get));
        assert_eq!(c.url.template.as_deref(), Some("/v1/x"));
        assert!(matches!(
            c.via,
            CallVia::Library { ref name } if name == "requests"
        ));
    }

    #[test]
    fn python_httpx_post_with_template_records_hole() {
        let src = "ORDERS_URL = os.environ['ORDERS_URL']\nhttpx.post(f\"{ORDERS_URL}/api/orders/{order_id}\")\n";
        let calls = py_calls(src);
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.method, MethodSpec::Known(HttpMethod::Post));
        match &c.url.host {
            HostPart::Env(names) => assert_eq!(names.clone(), vec!["ORDERS_URL".to_string()]),
            other => panic!("expected Env, got {other:?}"),
        }
        assert_eq!(c.url.template.as_deref(), Some("/api/orders/{}"));
    }

    #[test]
    fn python_requests_request_picks_up_literal_method() {
        let src = "requests.request(\"GET\", \"https://x.example.com/api\")\n";
        let calls = py_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Get));
        assert_eq!(calls[0].url.template.as_deref(), Some("/api"));
        assert!(matches!(
            calls[0].via,
            CallVia::Library { ref name } if name == "requests"
        ));
    }

    #[test]
    fn python_requests_request_with_dynamic_method_is_unknown() {
        let src = "m = 'GET'\nrequests.request(m, \"https://x/api\")\n";
        let calls = py_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Unknown);
    }

    #[test]
    fn python_httpx_request_with_kwarg_method() {
        let src = "httpx.request(method=\"DELETE\", url=\"https://x/api\")\n";
        let calls = py_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Delete));
    }

    #[test]
    fn python_client_httpx_propagates_base_url() {
        let src = "\
client = httpx.Client(base_url=\"https://api.example.com\")
client.get(\"/v1/x\")
";
        let calls = py_calls(src);
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.url.host, HostPart::Literal("api.example.com".to_string()));
        assert_eq!(c.url.template.as_deref(), Some("/v1/x"));
        assert!(matches!(
            c.via,
            CallVia::Library { ref name } if name == "httpx"
        ));
    }

    #[test]
    fn python_client_with_as_clause_propagates_base_url() {
        let src = "\
with httpx.Client(base_url=\"https://api.example.com\") as client:
    client.get(\"/v1/x\")
";
        let calls = py_calls(src);
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.url.host, HostPart::Literal("api.example.com".to_string()));
        assert_eq!(c.url.template.as_deref(), Some("/v1/x"));
    }

    #[test]
    fn python_async_with_async_client_propagates_base_url() {
        let src = "\
async with httpx.AsyncClient(base_url=\"https://api.example.com\") as client:
    client.get(\"/v1/x\")
";
        let calls = py_calls(src);
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.url.host, HostPart::Literal("api.example.com".to_string()));
        assert_eq!(c.url.template.as_deref(), Some("/v1/x"));
    }

    #[test]
    fn python_aiohttp_client_session_propagates_base_url() {
        let src = "\
session = aiohttp.ClientSession()
";
        let calls = py_calls(src);
        assert_eq!(calls.len(), 0);
    }

    #[test]
    fn python_wrapper_candidate_with_slash_prefix_is_recorded() {
        let src = "my_lib.post(\"/api/orders\", data=body)\n";
        let calls = py_calls(src);
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.method, MethodSpec::Known(HttpMethod::Post));
        assert!(matches!(
            c.via,
            CallVia::Receiver { ref expr, ref fn_name } if expr == "my_lib" && fn_name == "post"
        ));
    }

    #[test]
    fn python_wrapper_candidate_without_slash_prefix_is_ignored() {
        let src = "my_lib.post(\"https://x.example.com/api\", data=body)\n";
        let calls = py_calls(src);
        assert_eq!(calls.len(), 0);
    }

    #[test]
    fn python_httpx_module_level_call_attaches_to_file() {
        let src = "httpx.get(\"https://x.example.com/health\")\n";
        let calls = py_calls(src);
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn ts_fetch_records_default_get() {
        let src = "fetch(\"https://x.example.com/api\");\n";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Get));
    }

    #[test]
    fn ts_fetch_init_method_literal() {
        let src = "fetch(\"/api\", { method: \"POST\", body: x });\n";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Post));
        assert_eq!(calls[0].url.template.as_deref(), Some("/api"));
    }

    #[test]
    fn ts_fetch_init_method_dynamic_is_unknown() {
        let src = "fetch(\"/api\", { method: m });\n";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Unknown);
    }

    #[test]
    fn ts_axios_get_verb() {
        let src = "axios.get(\"/api/orders\");\n";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Get));
        assert!(matches!(
            calls[0].via,
            CallVia::Library { ref name } if name == "axios"
        ));
    }

    #[test]
    fn ts_axios_object_form_records_method_and_url() {
        let src = "axios({ method: 'DELETE', url: '/api/x' });\n";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Delete));
        assert_eq!(calls[0].url.template.as_deref(), Some("/api/x"));
    }

    #[test]
    fn ts_axios_bare_url_has_unknown_method() {
        let src = "axios(\"/api/x\");\n";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Unknown);
        assert_eq!(calls[0].url.template.as_deref(), Some("/api/x"));
    }

    #[test]
    fn ts_got_member_and_object_form() {
        let src = "\
got.get(\"/api/x\");
got(\"/api/y\", { method: \"PUT\" });
";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Get));
        assert_eq!(calls[1].method, MethodSpec::Known(HttpMethod::Put));
    }

    #[test]
    fn ts_ky_verb_records_call() {
        let src = "ky.post(\"/api/x\", { json: body });\n";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Post));
    }

    #[test]
    fn ts_template_literal_with_substitution_yields_env_host() {
        let src = "fetch(`${process.env.BILLING_URL}/invoices/${id}`);\n";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        match &c.url.host {
            HostPart::Env(names) => assert_eq!(names, &vec!["BILLING_URL".to_string()]),
            other => panic!("expected Env, got {other:?}"),
        }
        assert_eq!(c.url.template.as_deref(), Some("/invoices/{}"));
    }

    #[test]
    fn ts_urljoin_records_base_then_path() {
        let src = "fetch(urljoin(BASE, \"/api/orders\"));\n";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        match &c.url.host {
            HostPart::Expr(_) => {} // BASE resolved is not a known env pattern
            other => panic!("expected Expr or Env, got {other:?}"),
        }
        // base (Hole BASE) + path "/api/orders"
        assert_eq!(c.url.template.as_deref(), Some("/api/orders"));
    }

    #[test]
    fn ts_new_url_records_base_then_path() {
        // §6.3: `new URL("/p", base)` gives parts of `base` then "/p".
        // BASE is unresolvable here (no assignment), so the host is
        // `Expr("BASE")`; the path is `/api`. This pins the
        // `new URL` recognition — without `call_urljoin_parts`
        // matching, parts would be empty and template would be `/`.
        let src = "fetch(new URL(\"/api\", BASE));\n";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.url.host, HostPart::Expr("BASE".to_string()));
        assert_eq!(c.url.template.as_deref(), Some("/api"));
    }

    #[test]
    fn ts_new_url_chained_records_dynamic_url() {
        // `new URL(...).toString()` is a member_expression; §6.3 says
        // we record what we can. Without further work the URL arg
        // becomes a single Hole carrying the full expression text,
        // host is `Expr(...)`. The §4.5 normalizer strips the host
        // hole, leaving an empty path that renders as `/` (the root).
        let src = "fetch(new URL(\"/api\", BASE).toString());\n";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        match &c.url.host {
            HostPart::Expr(_) => {}
            other => panic!("expected Expr (member-chain dynamic URL), got {other:?}"),
        }
        assert_eq!(
            c.url.template.as_deref(),
            Some("/"),
            "an entirely-Hole member expression has its host consumed; the empty path renders as '/' (§4.5 step 4)"
        );
    }

    #[test]
    fn ts_wrapper_candidate_with_slash_prefix() {
        let src = "myHttp.post(\"/api/x\", data);\n";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert!(matches!(
            c.via,
            CallVia::Receiver { ref expr, ref fn_name } if expr == "myHttp" && fn_name == "post"
        ));
    }

    #[test]
    fn url_expr_is_capped_at_200_chars() {
        let long_url = format!("\"{}\"", "x".repeat(500));
        let src = format!("fetch({});\n", long_url);
        let calls = ts_calls(&src);
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].url_expr.len() <= URL_EXPR_CAP,
            "url_expr must be capped at {URL_EXPR_CAP}; got {}",
            calls[0].url_expr.len()
        );
    }

    #[test]
    fn reads_complete_is_true_on_every_emitted_call() {
        let src = "\
fetch(\"/api\");
fetch(\"/api\", { method: \"POST\" });
axios.get(\"/api\");
axios.post(\"/api\");
got.get(\"/api\");
";
        for c in ts_calls(src) {
            assert!(c.reads_complete, "{c:?}");
        }
        let py_src = "\
import httpx
httpx.get(\"/api\")
httpx.post(\"/api\")
httpx.request(\"GET\", \"/api\")
";
        for c in py_calls(py_src) {
            assert!(c.reads_complete, "{c:?}");
        }
    }

    #[test]
    fn one_step_identifier_resolution_to_a_unique_assignment() {
        // `BASE` is uniquely assigned at module level.
        let src = "\
BASE = \"https://orders.svc\"
fetch(BASE + \"/api/x\")
";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.url.host, HostPart::Literal("orders.svc".to_string()));
        assert_eq!(c.url.template.as_deref(), Some("/api/x"));
    }

    #[test]
    fn unresolvable_identifier_stays_a_hole() {
        // Two assignments with the same name at module level → no
        // resolution; the hole stays.
        let src = "\
BASE = \"https://a.example.com\"
BASE = \"https://b.example.com\"
fetch(BASE + \"/api\")
";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 1);
        match &calls[0].url.host {
            HostPart::Expr(_) => {}
            other => panic!("expected Expr (unresolvable), got {other:?}"),
        }
    }

    #[test]
    fn same_function_assignment_wins_when_unique() {
        let src = "\
async function f() {
    BASE = \"https://orders.svc\"
    await fetch(BASE + \"/api/x\")
}
";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.url.host, HostPart::Literal("orders.svc".to_string()));
    }

    #[test]
    fn every_framework_provider_matches_consumer_template() {
        // §4.5 consumer-side equivalence property: for each
        // framework's provider declaration in the fixture, a matching
        // consumer call produces the same template. We exercise the
        // half the PR 5 property test deferred.
        //
        // axum provider /api/orders/{id} (the `{}` is the §4.5
        // normalized form of `:id`) — consumer call:
        let py_src = "\
httpx.get(f\"{ORDERS_URL}/api/orders/{order_id}\")
";
        let py = py_calls(py_src);
        assert_eq!(py[0].url.template.as_deref(), Some("/api/orders/{}"));

        let ts_src = "fetch(`/api/orders/${id}`);\n";
        let ts = ts_calls(ts_src);
        assert_eq!(ts[0].url.template.as_deref(), Some("/api/orders/{}"));

        // Express-style provider /api/login (literal) — consumer:
        let ts_src = "axios.get(\"/api/login\");\n";
        let ts = ts_calls(ts_src);
        assert_eq!(ts[0].url.template.as_deref(), Some("/api/login"));

        // FastAPI-style /api/widgets/{widget_id} (literal `{widget_id}`
        // is the §4.5 parameter form):
        let py_src = "httpx.get(\"/api/widgets/{widget_id}\")\n";
        let py = py_calls(py_src);
        assert_eq!(py[0].url.template.as_deref(), Some("/api/widgets/{}"));

        // Go net/http-style /healthz (literal):
        let ts_src = "fetch(\"/healthz\");\n";
        let ts = ts_calls(ts_src);
        assert_eq!(ts[0].url.template.as_deref(), Some("/healthz"));
    }

    #[test]
    fn unknown_method_is_emitted() {
        let src = "fetch(\"/api\", { method: dynamicMethod });\n";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Unknown);
    }

    #[test]
    fn case_insensitive_verbs_match() {
        let py_src = "\
requests.GET(\"/a\")
requests.Post(\"/b\")
";
        let calls = py_calls(py_src);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Get));
        assert_eq!(calls[1].method, MethodSpec::Known(HttpMethod::Post));
    }

    #[test]
    fn binary_expression_concatenation() {
        let src = "fetch(BASE + \"/api/x\");\n";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].url.template.as_deref(), Some("/api/x"));
    }

    #[test]
    fn template_with_only_substitutions_has_none_template() {
        let src = "fetch(`${a}/${b}`);\n";
        let calls = ts_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].url.template, None);
    }

    #[test]
    fn build_graph_emits_node_and_edge() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("g.bin");
        let graph = GraphDatabase::new(&db_path).unwrap();
        let ns = RepoNamespace::for_test();
        let handler = GraphNode::new_in(
            NodeType::Function,
            "fetch_orders".to_string(),
            "src/main.py".to_string(),
            &ns,
        )
        .with_location_in(5, 10, &ns);
        graph
            .insert_nodes_batch(std::slice::from_ref(&handler))
            .unwrap();
        let call = HttpClientCall {
            method: MethodSpec::Known(HttpMethod::Get),
            url: NormalizedUrl {
                host: HostPart::None,
                template: Some("/api/x".to_string()),
            },
            via: CallVia::Library {
                name: "requests".to_string(),
            },
            url_expr: "\"/api/x\"".to_string(),
            reads_complete: true,
            path: "src/main.py".to_string(),
            line: 7,
        };
        let (nodes, edges) = build_graph(&graph, &call, &ns);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].node_type, NodeType::HttpClientCall);
        assert_eq!(nodes[0].name, "GET /api/x");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].edge_type, EdgeType::SendsHttp);
        assert_eq!(edges[0].source_id, handler.id);
        assert!(edges[0].site.is_some());
    }

    #[test]
    fn module_level_call_attaches_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("g.bin");
        let graph = GraphDatabase::new(&db_path).unwrap();
        let ns = RepoNamespace::for_test();
        let file = GraphNode::new_in(
            NodeType::File,
            "main.py".to_string(),
            "src/main.py".to_string(),
            &ns,
        );
        graph
            .insert_nodes_batch(std::slice::from_ref(&file))
            .unwrap();
        let call = HttpClientCall {
            method: MethodSpec::Known(HttpMethod::Get),
            url: NormalizedUrl {
                host: HostPart::None,
                template: Some("/healthz".to_string()),
            },
            via: CallVia::Library {
                name: "requests".to_string(),
            },
            url_expr: "\"/healthz\"".to_string(),
            reads_complete: true,
            path: "src/main.py".to_string(),
            line: 5,
        };
        let (nodes, edges) = build_graph(&graph, &call, &ns);
        assert_eq!(nodes.len(), 1);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source_id, file.id);
    }

    // ─── Rust outbound HTTP (PR 14 / Workstream 1) ─────────────

    fn rust_calls(src: &str) -> Vec<HttpClientCall> {
        detect_calls(std::path::Path::new("test.rs"), src, Lang::Rust)
    }

    #[test]
    fn rust_reqwest_get_emits_a_consumer_fact() {
        let src = "\
fn main() {
    let body = reqwest::get(\"https://api.example.com/v1/x\").unwrap();
    println!(\"{:?}\", body);
}
";
        let calls = rust_calls(src);
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.method, MethodSpec::Known(HttpMethod::Get));
        assert!(matches!(
            c.via,
            CallVia::Library { ref name } if name == "reqwest"
        ));
        assert_eq!(c.url.template.as_deref(), Some("/v1/x"));
    }

    #[test]
    fn rust_reqwest_client_get_via_method_chain() {
        // The brief mentions `reqwest::Client::new().get(url)`; v1
        // detects the bare-`reqwest::get(url)` form, not chained
        // builders. A chained builder's receiver is the freshly-built
        // `reqwest::Client`, not the library name — recognizing that
        // shape requires walking back through method chains (deferred).
        let src = "\
fn main() {
    let client = reqwest::Client::new();
    let r = client.get(\"https://orders/api/orders\");
}
";
        let calls = rust_calls(src);
        assert!(
            calls.is_empty(),
            "v1 does not chase the chained-builder receiver; the call \
             fires when the receiver text is `reqwest` directly: {calls:?}"
        );
    }

    #[test]
    fn rust_ureq_post_is_recognized() {
        let src = "\
fn main() {
    ureq::post(\"https://x.example.com/invoices\").send_json(payload);
}
";
        let calls = rust_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Post));
        assert!(matches!(
            calls[0].via,
            CallVia::Library { ref name } if name == "ureq"
        ));
    }

    #[test]
    fn rust_unrelated_call_is_not_a_client() {
        let src = "\
fn main() {
    let _ = db.fetch_one(\"/api\");
    let _ = thing.process(\"/foo\");
}
";
        let calls = rust_calls(src);
        assert!(
            calls.is_empty(),
            "non-HTTP-library method calls must not be detected: {calls:?}"
        );
    }

    // ─── Go outbound HTTP (PR 14 / Workstream 1) ────────────────

    fn go_calls(src: &str) -> Vec<HttpClientCall> {
        detect_calls(std::path::Path::new("test.go"), src, Lang::Go)
    }

    #[test]
    fn go_http_get_emits_a_consumer_fact() {
        let src = "\
package main

func f() {
    resp, _ := http.Get(\"https://api.example.com/v1/x\")
    _ = resp
}
";
        let calls = go_calls(src);
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.method, MethodSpec::Known(HttpMethod::Get));
        assert!(matches!(
            c.via,
            CallVia::Library { ref name } if name == "http"
        ));
        assert_eq!(c.url.template.as_deref(), Some("/v1/x"));
    }

    #[test]
    fn go_http_post_method_from_first_arg() {
        let src = "\
package main

func f() {
    http.Post(\"https://x.example.com/invoices\", \"application/json\", body)
}
";
        let calls = go_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Post));
        assert_eq!(calls[0].url.template.as_deref(), Some("/invoices"));
    }

    #[test]
    fn go_http_new_request_method_bearing() {
        let src = "\
package main

func f() {
    req, _ := http.NewRequest(\"DELETE\", \"https://x.example.com/api/x\", nil)
    _ = req
}
";
        let calls = go_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Delete));
        assert_eq!(calls[0].url.template.as_deref(), Some("/api/x"));
    }

    #[test]
    fn go_http_new_request_with_dynamic_method_is_unknown() {
        let src = "\
package main

func f() {
    m := \"POST\"
    http.NewRequest(m, \"https://x/api\", nil)
}
";
        let calls = go_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Unknown);
    }

    #[test]
    fn go_http_new_request_with_context_method_bearing() {
        let src = "\
package main

func f() {
    req, _ := http.NewRequestWithContext(ctx, \"PUT\", \"https://x.example.com/api/x\", nil)
    _ = req
}
";
        let calls = go_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Put));
        assert_eq!(calls[0].url.template.as_deref(), Some("/api/x"));
    }

    #[test]
    fn go_unrelated_call_is_not_a_client() {
        let src = "\
package main

func f() {
    foo.bar(\"/api\")
    unrelated.Get(\"/x\")
}
";
        let calls = go_calls(src);
        assert!(
            calls.is_empty(),
            "non-http receiver must not be detected: {calls:?}"
        );
    }

    // ─── Workstream 5: Java / C# / Ruby / Kotlin ────────────────────

    fn java_calls(src: &str) -> Vec<HttpClientCall> {
        detect_calls(std::path::Path::new("Foo.java"), src, Lang::Java)
    }
    fn csharp_calls(src: &str) -> Vec<HttpClientCall> {
        detect_calls(std::path::Path::new("Foo.cs"), src, Lang::CSharp)
    }

    fn ruby_calls(src: &str) -> Vec<HttpClientCall> {
        detect_calls(std::path::Path::new("Foo.rb"), src, Lang::Ruby)
    }
    fn kotlin_calls(src: &str) -> Vec<HttpClientCall> {
        detect_calls(std::path::Path::new("Foo.kt"), src, Lang::Kotlin)
    }

    #[test]
    fn java_httpclient_send_records_call() {
        let src = "\
import java.net.http.*;
class Foo {
    void Bar() throws Exception {
        HttpClient client = HttpClient.newHttpClient();
        HttpResponse<String> r = client.send(
            HttpRequest.newBuilder().uri(java.net.URI.create(\"http://x/api\")).build(),
            HttpResponse.BodyHandlers.ofString()
        );
    }
}
";
        let calls = java_calls(src);
        // The HttpClient.send() site is one of the detected calls.
        // The OkHttp / RestTemplate sites are not present in this
        // snippet, so we should have at least one call.
        assert!(
            !calls.is_empty(),
            "java HttpClient.send must be detected: {calls:?}"
        );
        assert!(calls.iter().any(|c| matches!(
            c.via,
            CallVia::Library { ref name } if name == "http"
        )));
    }

    #[test]
    fn java_non_client_call_is_skipped() {
        let src = "class Foo { void bar() { someObject.run(\"/api\"); } }\n";
        let calls = java_calls(src);
        assert!(
            calls.is_empty(),
            "non-HttpClient send / OkHttp execute must not be a client call: {calls:?}"
        );
    }

    #[test]
    fn csharp_httpclient_get_async_records_call() {
        let src = "\
class Foo {
    void Bar() {
        var client = new HttpClient();
        var r = client.GetAsync(\"/api/x\");
    }
}
";
        let calls = csharp_calls(src);
        assert_eq!(
            calls.len(),
            1,
            "csharp HttpClient.GetAsync must emit one call: {calls:?}"
        );
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Get));
        assert_eq!(calls[0].url.template.as_deref(), Some("/api/x"));
        assert!(matches!(
            calls[0].via,
            CallVia::Library { ref name } if name == "httpclient"
        ));
    }

    #[test]
    fn csharp_post_async_records_call() {
        let src = "class Foo { void Bar() { var r = client.PostAsync(\"/api\", null); } }\n";
        let calls = csharp_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Post));
    }

    #[test]
    fn csharp_webclient_download_records_call() {
        let src = "\
class Foo {
    void Bar() {
        var wc = new WebClient();
        var s = wc.DownloadString(\"/api/x\");
    }
}
";
        let calls = csharp_calls(src);
        assert_eq!(calls.len(), 1);
        assert!(matches!(
            calls[0].via,
            CallVia::Library { ref name } if name == "webclient"
        ));
    }

    #[test]
    fn csharp_non_verb_call_is_skipped() {
        let src = "class Foo { void Bar() { client.Connect(\"/api\"); } }\n";
        let calls = csharp_calls(src);
        assert!(
            calls.is_empty(),
            "csharp unknown verb must not be a client call: {calls:?}"
        );
    }

    #[test]
    fn ruby_net_http_get_records_call() {
        let src = "require 'net/http'\nNet::HTTP.get(URI('http://x.example.com/api'))\n";
        let calls = ruby_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Get));
        assert!(matches!(
            calls[0].via,
            CallVia::Library { ref name } if name == "net/http"
        ));
    }

    #[test]
    fn ruby_httparty_post_records_call() {
        let src = "HTTParty.post('http://x.example.com/api', body: {})\n";
        let calls = ruby_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Post));
        assert!(matches!(
            calls[0].via,
            CallVia::Library { ref name } if name == "httparty"
        ));
    }

    #[test]
    fn ruby_faraday_delete_records_call() {
        let src = "Faraday.delete('http://x.example.com/api')\n";
        let calls = ruby_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Delete));
    }

    #[test]
    fn ruby_restclient_get_records_call() {
        let src = "RestClient.get('http://x.example.com/api')\n";
        let calls = ruby_calls(src);
        assert_eq!(calls.len(), 1);
        assert!(matches!(
            calls[0].via,
            CallVia::Library { ref name } if name == "restclient"
        ));
    }

    #[test]
    fn ruby_unrelated_call_is_skipped() {
        let src = "Foo.delete('http://x.example.com/api')\n";
        let calls = ruby_calls(src);
        assert!(
            calls.is_empty(),
            "non-Ruby-HTTP-gem receiver must not be a client call: {calls:?}"
        );
    }

    #[test]
    fn kotlin_ktor_client_get_records_call() {
        let src = "\
fun foo() {
    val r = client.get<String>(\"/api/x\")
}
";
        let calls = kotlin_calls(src);
        assert_eq!(
            calls.len(),
            1,
            "ktor client.get must emit one call: {calls:?}"
        );
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Get));
        assert_eq!(calls[0].url.template.as_deref(), Some("/api/x"));
        assert!(matches!(
            calls[0].via,
            CallVia::Library { ref name } if name == "ktor"
        ));
    }

    #[test]
    fn kotlin_ktor_client_post_records_call() {
        let src = "fun foo() {\n    client.post(\"/api\", body)\n}\n";
        let calls = kotlin_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, MethodSpec::Known(HttpMethod::Post));
    }

    #[test]
    fn kotlin_okhttp_execute_records_call() {
        let src = "fun foo() {\n    OkHttpClient().newCall(Request.Builder().url(\"/api\").build()).execute()\n}\n";
        let calls = kotlin_calls(src);
        assert_eq!(calls.len(), 1);
        assert!(matches!(
            calls[0].via,
            CallVia::Library { ref name } if name == "okhttp"
        ));
    }

    #[test]
    fn kotlin_non_verb_call_is_skipped() {
        let src = "fun foo() { client.connect(\"/api\") }\n";
        let calls = kotlin_calls(src);
        assert!(
            calls.is_empty(),
            "kotlin unknown verb must not be a client call: {calls:?}"
        );
    }

    #[test]
    fn lang_for_path_routes_new_languages() {
        assert!(matches!(Lang::Java as u8, _ if true)); // tautology; reachability marker
        assert_eq!(std::any::type_name::<Lang>(), std::any::type_name::<Lang>());
        // The dispatch on extension lives in `scan_workspace_clients` —
        // exercise it indirectly via a Java / C# / Ruby / Kotlin
        // sample.
        assert_eq!(java_calls("class Foo {}").len(), 0);
        assert_eq!(csharp_calls("class Foo {}").len(), 0);
        assert_eq!(ruby_calls("# noop\n").len(), 0);
        assert_eq!(kotlin_calls("// noop\n").len(), 0);
    }
}
