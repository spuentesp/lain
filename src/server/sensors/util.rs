//! Shared helpers for protocol sensors.
//!
//! Sensors register via [`crate::server::sensors::Sensor`]; everything
//! here is imported, never re-implemented per sensor.

use crate::graph::GraphDatabase;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
use std::path::Path;
use tree_sitter::{Language, Parser, Tree};

// Forward the patterns module into this file's scope so the deny-list
// accessor below can read from `Patterns::patterns()` without forcing
// every sensor to re-import `crate::server::sensors::patterns`.
use crate::server::sensors::patterns::Patterns;

// ─── Repo identity ─────────────────────────────────────────────

/// Fallback `RepoId` for a sensor whose caller could not supply one.
///
/// Deliberately a single neutral token. A `RepoId` must never be a
/// sensor name: `SymbolKey.repo` is rendered as an evidence field
/// (`diff_contracts`' `handlers[].repo`) and an external client
/// resolves it as a repository. Note that `RepoId::new` rejects any
/// value containing `/`, so `RepoId::new(root.to_string_lossy())`
/// fails for every real workspace path and this fallback is the
/// common case, not an edge case.
pub fn fallback_repo_id() -> crate::federation::repo_id::RepoId {
    crate::federation::repo_id::RepoId::new("unknown").expect("'unknown' is a valid RepoId")
}

// ─── Language classification ────────────────────────────────────

/// Source-file language for sensors that walk code. PR 14 added Rust +
/// Go; PR 16 (Workstream 5) adds Java, C#, Ruby, Kotlin. The shared
/// [`lang_for_path`] and [`parse_for_lang`] helpers (per sensor
/// wrapper) keep every sensor's per-language walker pointed at the
/// right tree-sitter grammar without duplicating the lookup table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Python,
    TsJs,
    Ts,
    Tsx,
    Rust,
    Go,
    Java,
    CSharp,
    Ruby,
    Kotlin,
}

/// Map a source-file path's extension to a [`Lang`]. `None` for files
/// no sensor in this crate parses. The order is fixed (it's the
/// identity test the §8.3 determinism contract checks for).
pub fn lang_for_path(path: &str) -> Option<Lang> {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "py" => Some(Lang::Python),
        "ts" => Some(Lang::Ts),
        "tsx" => Some(Lang::Tsx),
        "js" | "jsx" | "mjs" | "cjs" => Some(Lang::TsJs),
        "rs" => Some(Lang::Rust),
        "go" => Some(Lang::Go),
        "java" => Some(Lang::Java),
        "cs" => Some(Lang::CSharp),
        "rb" => Some(Lang::Ruby),
        "kts" | "kt" => Some(Lang::Kotlin),
        _ => None,
    }
}

/// Build a tree-sitter [`Tree`] for `lang`. Each sensor's per-language
/// parser consumes this — none of them re-implement the grammar
/// lookup. Returns `None` when the grammar refuses to set (shouldn't
/// happen for the ten languages above) or the parser returns no tree.
pub fn parse_for_lang(lang: Lang, src: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    let grammar: Language = grammar_for(lang);
    parser.set_language(&grammar).ok()?;
    parser.parse(src, None)
}

/// Resolve the tree-sitter [`Language`] for `lang` without parsing.
/// Used by the http_sensor's tree-sitter walker (Task 2 §3) to
/// compile `.scm` queries via `tree_sitter::Query::new` before
/// running them against a parsed tree.
pub fn language_for(lang: Lang) -> Language {
    grammar_for(lang)
}

fn grammar_for(lang: Lang) -> Language {
    match lang {
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::TsJs => tree_sitter_javascript::LANGUAGE.into(),
        Lang::Ts => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        Lang::Go => tree_sitter_go::LANGUAGE.into(),
        Lang::Java => tree_sitter_java::LANGUAGE.into(),
        Lang::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
        Lang::Ruby => tree_sitter_ruby::LANGUAGE.into(),
        Lang::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
    }
}

// ─── Response-metadata denylist (Workstream 5, §6.5) ─────────────

/// `true` when `key` names a response-metadata accessor (body parser,
/// status / status-text, …) for any bound HTTP client the language
/// produces. Bug B of the contract-federation bug fixes; the
/// `field_access_sensor`'s attribute walker suppresses `ReadsField`
/// emission when the attribute on a bound receiver matches.
///
/// Pre-refactor (Workstream 5, PR 13/14/16), the same policy lived in
/// the inline `RESPONSE_METHOD_DENYLIST` constant in this file. Task 5
/// of the data-driven sensor-patterns plan relocated the deny list
/// into `frameworks.yaml` under each language's outbound entry; this
/// accessor is the single sink the sensor now consults. The deny set
/// is data-driven and runtime-overridable via `<root>/.lain/patterns/`
/// — see [`crate::server::sensors::patterns::Patterns::load_overrides`].
///
/// The per-language union matches the old list for every language the
/// sensor supports, so existing walker tests stay green.
///
/// **Per-scan override wiring:** when a sensor's `scan_workspace_*`
/// function has loaded per-repo overrides (via
/// [`crate::server::sensors::patterns::Patterns::with_overrides`]),
/// it sets the thread-local current patterns via
/// [`with_current_patterns`]. The walker code (which can't easily
/// thread a `&Patterns` through every helper — `handle_attribute`,
/// `handle_ruby_node`, etc., are 8 deep and are called from
/// recursive tree-sitter walks) reads from this thread-local. When
/// the thread-local is unset, this falls back to the bundled
/// singleton (`Patterns::patterns()`), preserving the no-override
/// baseline.
pub fn is_deny_method(lang: Lang, key: &str) -> bool {
    let patterns: &'static Patterns = current_patterns().unwrap_or_else(Patterns::patterns);
    patterns
        .outbound_patterns(lang)
        .any(|def| def.deny_methods.iter().any(|m| m == key))
}

// Thread-local `Patterns` reference for the currently-running
// sensor's scan. [`with_current_patterns`] (used by the four
// `scan_workspace_*` functions) sets it at the top of each scan and
// clears it at the bottom. Helper functions like [`is_deny_method`]
// read from here so the override-augmented deny set is visible to
// deep walker helpers that can't easily accept a `&Patterns`
// parameter.
thread_local! {
    static CURRENT_PATTERNS: std::cell::RefCell<*const Patterns> =
        const { std::cell::RefCell::new(std::ptr::null()) };
}

/// Read the thread-local `&Patterns` if one was installed via
/// [`with_current_patterns`] for the current sensor scan. Returns
/// `None` for callers outside a scan (e.g., lib tests that exercise
/// the walker without going through `scan_workspace_*`).
pub fn current_patterns() -> Option<&'static Patterns> {
    CURRENT_PATTERNS.with(|c| {
        let ptr = *c.borrow();
        if ptr.is_null() {
            None
        } else {
            // SAFETY: The pointer was installed by
            // `with_current_patterns` for the duration of a single
            // `scan_workspace_*` call. The `Patterns` `f` outlives
            // the scan (it's a local in the caller), and the
            // thread-local is cleared before `f` is dropped, so
            // any read of the thread-local pointer is well-defined.
            Some(unsafe { &*ptr })
        }
    })
}

/// Run `f` with `patterns` installed as the thread-local current
/// patterns. The thread-local is cleared on return regardless of
/// `f`'s outcome (panic, early return, normal completion).
pub fn with_current_patterns<R>(patterns: &Patterns, f: impl FnOnce() -> R) -> R {
    let prev = CURRENT_PATTERNS.with(|c| {
        let prev = *c.borrow();
        *c.borrow_mut() = patterns as *const Patterns;
        prev
    });
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    CURRENT_PATTERNS.with(|c| *c.borrow_mut() = prev);
    match result {
        Ok(v) => v,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// A file found by [`walk_workspace`].
pub struct WalkedFile(std::path::PathBuf);

impl WalkedFile {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// Extensions the route-shaped sensors (http, entry points) scan.
pub const SOURCE_EXTS: &[&str] = &[
    "rs", "py", "ts", "js", "go", "java", "cs", "rb", "kt", "kts",
];

/// `path`'s extension when it is one of `exts` (as spelled in `exts`).
pub fn ext_in(path: &Path, exts: &'static [&'static str]) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?;
    exts.iter().copied().find(|e| *e == ext)
}

/// [`walk_workspace`] plus the per-file boilerplate every sensor repeats:
/// `select` picks the files to scan (and a per-file tag such as the
/// extension or language), then the file is read. Files `select` skips
/// are never read; one that vanished or is not UTF-8 between the walk
/// and the read is skipped silently. Yields `(path, content, tag)`.
pub fn scan_files<'a, T: 'a>(
    root: &'a Path,
    select: impl Fn(&Path) -> Option<T> + 'a,
) -> impl Iterator<Item = (std::path::PathBuf, String, T)> + 'a {
    walk_workspace(root).filter_map(move |entry| {
        let path = entry.path();
        let tag = select(path)?;
        let content = std::fs::read_to_string(path).ok()?;
        Some((path.to_path_buf(), content, tag))
    })
}

/// Walk every file under `root`: what the main scan indexes, so a sensor
/// sees the same code the graph has. That is every path the walker yields
/// honouring `.gitignore` and hidden-file rules, plus every file git
/// tracks even though `.gitignore` matches it (generated code that is
/// checked in) — skipping those left routes defined there without
/// `CallsHttp` edges although their handlers were indexed. Per-entry errors
/// are flattened away — a malformed symlink or a target that vanishes
/// mid-walk must not abort ingestion.
pub fn walk_workspace(root: &Path) -> impl Iterator<Item = WalkedFile> {
    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    let mut files: Vec<std::path::PathBuf> = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .build()
        .flatten()
        .map(|e| e.into_path())
        .inspect(|p| {
            seen.insert(p.clone());
        })
        .collect();
    if let Ok(repo) = git2::Repository::open(root) {
        if let (Ok(index), Some(workdir)) = (repo.index(), repo.workdir()) {
            // Compare canonical forms (macOS `/var` → `/private/var`,
            // Windows `\\?\`), but report paths under `root` as the caller
            // spelled it, so they match the walk above.
            let canon = |p: &Path| dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            let (canon_root, canon_wd) = (canon(root), canon(workdir));
            for entry in index.iter() {
                let rel = String::from_utf8_lossy(&entry.path).to_string();
                // Hidden paths stay out, as in the walk above.
                if rel.split('/').any(|c| c.starts_with('.')) {
                    continue;
                }
                let Ok(below_root) = canon_wd
                    .join(&rel)
                    .strip_prefix(&canon_root)
                    .map(Path::to_path_buf)
                else {
                    continue;
                };
                let path = root.join(below_root);
                if path.is_file() && !seen.contains(&path) {
                    seen.insert(path.clone());
                    files.push(path);
                }
            }
        }
    }
    files.into_iter().map(WalkedFile)
}

/// `CamelCase` / `mixedCase` → `snake_case`. Each non-initial uppercase
/// char introduces a `_` separator.
pub fn to_snake_case(name: &str) -> String {
    let mut result = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            result.push('_');
        }
        result.push(c.to_ascii_lowercase());
    }
    result
}

/// `snake_case` → `camelCase`. The first non-`_` char stays as-is;
/// each subsequent `_`-delimited segment is title-cased.
pub fn to_camel_case(name: &str) -> String {
    let mut result = String::new();
    let mut capitalize = false;
    for c in name.chars() {
        if c == '_' {
            capitalize = true;
        } else if capitalize {
            result.push(c.to_ascii_uppercase());
            capitalize = false;
        } else {
            result.push(c);
        }
    }
    result
}

/// Compose a proto package-qualified service name from its parts.
/// Returns the bare `service` when `package` is empty (matches
/// the proto convention `service Foo { … }` for `package;`-less
/// files); otherwise `package.service`. Shared by the proto
/// provider sensor and the gRPC consumer sensor (Phase A review
/// §D2) — previously each sensor carried an identical 3-line
/// copy that could drift if the convention ever changes.
pub fn compose_service_name(package: &str, service: &str) -> String {
    if package.is_empty() {
        service.to_string()
    } else {
        format!("{}.{}", package, service)
    }
}

/// Extract the env-var name from a host-shaped expression.
/// Recognises the patterns the http_client_sensor's `host_env_name`
/// already knew about plus the same patterns the contract-federation
/// `clients.rs::extract_process_env_name` did (Phase C). Single
/// source of truth so the two scanners stay in sync (Phase B-D
/// review §S10).
///
/// Returns `None` for expressions that don't match any of the
/// known shapes — callers are expected to fall back to
/// `HostPart::Expr` when this returns `None`.
///
/// The grammar:
/// - `os.environ["X"]` / `os.environ.get("X", …)` / `os.getenv("X", …)`
/// - `process.env.X` / `process.env["X"]`
/// - `settings.X` / `config.X`
///
/// The function does not strip trailing punctuation — callers
/// that pass through the raw value of an object-literal field
/// (e.g. `process.env.X,` after the `baseURL:` prefix) must
/// `trim_end_matches` the trailing `,` / `)` / `}` / `;`
/// themselves before calling.
pub fn host_env_name(text: &str) -> Option<&str> {
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

/// Strip the surrounding `[ … ]` brackets and any quotes / spaces
/// around the inner literal. Returns the substring between the
/// outermost brackets.
fn inner_bracket(s: &str) -> &str {
    let t = s.trim();
    let bytes = t.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        return &t[1..t.len() - 1];
    }
    t
}

/// Strip the surrounding `( … )` parens and any quotes / spaces
/// around the inner literal. Returns the substring between the
/// outermost parens.
fn inner_paren(s: &str) -> &str {
    let t = s.trim();
    let bytes = t.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        return &t[1..t.len() - 1];
    }
    t
}

/// Look up a handler node by `name`, trying exact, then `snake_case`,
/// then `camelCase`. Empty `name` returns `None`.
///
/// Unifying the priority to `exact → snake → camel` is a strict
/// superset of proto's old `exact → snake` (proto gains camelCase
/// matches) and graphql's old `exact → camel → snake`. In practice the
/// three orderings agree on every input that doesn't start with `_`,
/// because `to_camel_case(x)` of a string with no `_` returns `x`
/// unchanged — so `camel` and `snake` swap positions without changing
/// the resolved set. The only realistic divergence is when both a
/// snake-case and a camelCase form exist as separate nodes; the new
/// order picks snake. The audit's case-mismatch incidents all turned
/// on missing one of the forms, not on choosing between them.
pub fn find_handler_in_graph(graph: &GraphDatabase, name: &str) -> Option<GraphNode> {
    if name.is_empty() {
        return None;
    }
    graph
        .find_node_by_name(name)
        .or_else(|| graph.find_node_by_name(&to_snake_case(name)))
        .or_else(|| graph.find_node_by_name(&to_camel_case(name)))
}

/// The `Function` or `Method` in `path` with the smallest range
/// `line_start..=line_end` that contains `line` (§6.1).
///
/// Ties (two symbols span the same number of lines) go to the
/// symbol whose `line_start` is later — a nested function is more
/// specific than the module-level one that wraps it. Returns
/// `None` if `path` is not in the graph, no symbol in it covers
/// `line`, or every candidate is a non-function/non-method node
/// (`SendsHttp` and `Calls` attach to enclosing functions, never to
/// the file node itself).
pub fn enclosing_symbol(graph: &GraphDatabase, path: &str, line: u32) -> Option<GraphNode> {
    graph
        .get_nodes_by_types(&[
            crate::schema::NodeType::Function,
            crate::schema::NodeType::Method,
        ])
        .ok()
        .into_iter()
        .flatten()
        .filter(|n| n.path == path)
        .filter_map(|n| match (n.line_start, n.line_end) {
            (Some(s), Some(e)) if s <= line && e >= line => Some((n, e.saturating_sub(s))),
            _ => None,
        })
        .min_by(|a, b| {
            // Smallest range first; tie → later line_start.
            a.1.cmp(&b.1)
                .then_with(|| b.0.line_start.cmp(&a.0.line_start))
        })
        .map(|(n, _)| n)
}

// ─── Shared emitters for the protocol sensors ──────────────────────

/// Prefix for synthetic nodes carrying a per-site consumer fact.
///
/// A sensor must never put its `ContractFact` on a node another sensor
/// owns — `replace_sensor_output` step 2 deletes an owner's nodes *and
/// their incident edges* (`graph/mod.rs:896`), so a shared symbol node
/// means one sensor's rescan silently destroys another's edges. Each
/// protocol that annotates a source site gets its own synthetic node
/// named `<prefix><path>:<line>`.
pub const SQL_READ_PREFIX: &str = "sql-read:";
/// See [`SQL_READ_PREFIX`]. Carries `TopicConsumerFact`.
pub const TOPIC_READ_PREFIX: &str = "topic-read:";
/// See [`SQL_READ_PREFIX`]. Carries `RpcConsumerFact`.
pub const RPC_CALL_PREFIX: &str = "rpc-call:";
/// See [`SQL_READ_PREFIX`]. Carries `GraphqlConsumerFact`.
pub const GRAPHQL_CALL_PREFIX: &str = "graphql-call:";
/// See [`SQL_READ_PREFIX`]. Carries `RpcHandlerFact` on a `Module` node.
pub const RPC_HANDLER_PREFIX: &str = "rpc-handler:";
/// See [`SQL_READ_PREFIX`]. Carries `GraphqlHandlerFact` on a `Module` node.
pub const GRAPHQL_HANDLER_PREFIX: &str = "graphql-handler:";
/// See [`SQL_READ_PREFIX`]. Carries `WebSocketConsumerFact` on an
/// `HttpClientCall` node.
pub const WS_CLIENT_PREFIX: &str = "ws:client:";
/// See [`SQL_READ_PREFIX`]. Carries `WebSocketProviderFact` on an
/// `HttpRoute` node.
pub const WS_SERVER_PREFIX: &str = "ws:server:";

/// Build the synthetic per-site node every consumer sensor emits: one
/// `Function`-typed node named `<prefix><path>:<line>`. The caller
/// attaches its `ContractFact` — most sites carry one, but
/// `event_sensor`'s producer edge-anchor does not, so the shape and the
/// content are separated here rather than forced into one signature.
///
/// `line_end` is deliberately `None` and must stay that way:
/// [`enclosing_symbol`] requires both bounds, so a synthetic node with
/// `line_end` set wins its `min_by` (a zero-width range beats the real
/// enclosing function) and steals every peer sensor's edge anchor. The
/// `id` is derived from `id_name`, so the name prefix that
/// `sensor_owner_of`'s ownership guard matches is part of the identity
/// — a node can never be both synthetic-named and collision-prone with
/// a real symbol.
pub fn synthetic_site_node(
    id_name: String,
    path: &str,
    line: u32,
    namespace: &crate::schema::RepoNamespace,
) -> crate::schema::GraphNode {
    let mut node = crate::schema::GraphNode::new(
        crate::schema::NodeType::Function,
        id_name.clone(),
        path.to_string(),
    );
    node.id = crate::schema::GraphNode::generate_id(
        &crate::schema::NodeType::Function,
        path,
        &id_name,
        Some(line),
        namespace,
    );
    node.line_start = Some(line);
    node.line_end = None;
    node
}
//
// The graphql consumer sensor used to duplicate this 25-line block
// at two call sites (SDL-derived consumers, then code-derived
// consumers); the two copies drifted only in the data they
// received, never in the emission shape. This helper is the single
// source of truth so the two call sites cannot diverge again.

use crate::federation::contracts::model::{
    ContractFact, FieldReadFact, FieldReadOrigin, GraphqlOp, JsonPath, PathSegment,
};

// ─── Tier-1 line-idiom walker (Task 8) ──────────────────────────────
//
// Pre-Tier-1, the event_sensor and websocket_sensor each carried
// ~150 lines of hardcoded per-framework detection logic (kafkajs /
// aiokafka / rdkafka / kafka-go / Celery / NestJS /
// `app.ws` / `new WebSocket` / `onopen = …`). The new walker is
// the single line-idiom consumer: it takes a list of pre-compiled
// regexes (built from `frameworks.yaml` entries) and a line of
// source, and yields the captured value plus an opaque
// `framework_id` so the caller can attribute the match back to
// the YAML entry that produced it. Detection is a pure data
// change; the per-kind projection (`Topic` node, `Produces` edge,
// `HttpRoute` with `WebSocketProvider` fact, …) stays in Rust.
//
// The walker compiles each entry's `path_regex` exactly once (a
// `Vec<CompiledIdiom>` the caller builds once per scan) so
// repeated calls across files do not re-run the DFA minimisation.

/// One pre-compiled framework entry, ready for the walker to apply.
/// `kind` is the `FrameworkKind` discriminator (the walker uses
/// it only to route the capture into the right field of the
/// returned [`IdiomMatch`]; it does not filter on `kind`).
pub struct CompiledIdiom {
    pub kind: crate::server::sensors::patterns::FrameworkKind,
    pub id: String,
    regex: regex::Regex,
}

impl CompiledIdiom {
    fn from_def(def: &crate::server::sensors::patterns::FrameworkDef) -> Option<Self> {
        let path_re = def.path_regex.as_deref()?;
        let regex = regex::Regex::new(path_re).ok()?;
        Some(Self {
            id: def.id.clone(),
            kind: def.kind,
            regex,
        })
    }
}

/// One match the walker yielded for a line. `literal` and
/// `identifier` are the two named slots the YAML's regex
/// captures. The walker tracks which group matched so the
/// caller can decide how to interpret it: a literal slot
/// captures a string-literal value (e.g. `'orders'`); an
/// identifier slot captures a bare identifier (e.g. `TOPIC_NAME`)
/// that may or may not resolve to a string via the same-file
/// constant table.
///
/// A single YAML entry's regex can declare both slots
/// `(?:["'](LITERAL)["']|IDENT)` so one entry covers both
/// idiomatic shapes. The walker populates whichever matched;
/// both `None` is impossible because the regex would not have
/// matched at all.
///
/// For entries whose regex declares a single capture group
/// (e.g. WebSocket client URLs), only one of the two slots is
/// populated per match — the YAML author picks which one
/// (the `compile_idioms` helper tags the slot based on the
/// entry's `kind` and the `group_roles` table below).
///
/// `line` is the 1-based source line the caller stamped before
/// invoking the walker; the per-line walker does not know it
/// itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdiomMatch {
    pub framework_id: String,
    pub kind: crate::server::sensors::patterns::FrameworkKind,
    /// String-literal capture (e.g. the contents of `"orders"`).
    /// Populated when the matched regex's literal-form group
    /// captured a non-empty value.
    pub literal: Option<String>,
    /// Bare-identifier capture (e.g. `TOPIC_NAME`). Populated
    /// when the matched regex's identifier-form group captured
    /// a non-empty value.
    pub identifier: Option<String>,
    /// 1-based source line the caller stamped before invoking
    /// the walker. Carried on the match so the caller's emission
    /// step does not have to thread `line_no` through every
    /// helper.
    pub line: u32,
}

impl IdiomMatch {
    /// Convenience accessor: the literal slot if present,
    /// otherwise the identifier slot. The two slots are
    /// mutually exclusive (a single match can populate at most
    /// one), so this is the "the value of this match" form
    /// when the caller does not care whether the value was a
    /// literal or an identifier (e.g. the WebSocket client URL
    /// case — both are valid URLs).
    pub fn value(&self) -> Option<&str> {
        self.literal.as_deref().or(self.identifier.as_deref())
    }
}

/// Per-entry mapping of capture-group index → slot. The walker
/// reads this to populate `IdiomMatch::literal` vs
/// `IdiomMatch::identifier` correctly. The convention encoded
/// here is the one `frameworks.yaml` follows:
///
/// - `TopicProducer` / `TopicConsumer`: group 1 is the literal,
///   group 2 is the identifier (when the regex has both).
/// - `Scheduled`: group 1 is the spec literal. The Celery
///   marker regex has no group; the walker tags the empty
///   match as `literal: None, identifier: Some("")` so the
///   caller's `schedule_value` falls back to the function-
///   lookahead path.
/// - `WebSocketClient` / `WebSocketServer`: group 1 is the
///   literal (URL / route).
/// - `WebSocketHandler`: group 1 is the event name (kept in
///   the regex for the alternation), group 2 is the handler
///   name. The walker tags group 1 as `literal` and group 2
///   as `identifier`; the websocket caller reads
///   `identifier` as the handler name.
fn group_roles(kind: crate::server::sensors::patterns::FrameworkKind) -> &'static [(usize, Slot)] {
    use crate::server::sensors::patterns::FrameworkKind as K;
    match kind {
        K::WebSocketHandler => &[(1, Slot::Literal), (2, Slot::Identifier)],
        K::TopicProducer | K::TopicConsumer | K::Scheduled => {
            &[(1, Slot::Literal), (2, Slot::Identifier)]
        }
        K::WebSocketClient | K::WebSocketServer => &[(1, Slot::Literal)],
        // Not used by the Tier-1 walker.
        K::Route | K::Outbound | K::EntryPoint => &[],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Literal,
    Identifier,
}

/// Generic Tier-1 line-idiom walker. Applies every compiled
/// idiom to `line` and returns the captures, classified by
/// slot (literal vs identifier). The walker is per-line (the
/// per-line shape is the same in every Tier-1 consumer); the
/// caller drives the line loop and the `strip_line_comment`
/// step.
///
/// `idioms` is borrowed because the typical caller builds it
/// once per scan (or once per process — the regex compilation
/// is the only allocation) and reuses it across every line of
/// every file.
///
/// A match is yielded whenever the regex matches, even if
/// every capture group is empty. This is the Celery
/// `@app.task` / `@shared_task` case: the regex has no
/// capture group, and the caller's `schedule_value` falls
/// through to the function-lookahead path. Without this
/// "yield on regex match" rule, the walker would never
/// surface a Celery match and the joiner would never see a
/// `scheduled` Topic.
pub fn walk_idioms(idioms: &[CompiledIdiom], line: &str, line_no: u32) -> Vec<IdiomMatch> {
    let mut out: Vec<IdiomMatch> = Vec::new();
    for idiom in idioms {
        let roles = group_roles(idiom.kind);
        for cap in idiom.regex.captures_iter(line) {
            let mut literal: Option<String> = None;
            let mut identifier: Option<String> = None;
            for (group_idx, slot) in roles {
                if let Some(m) = cap.get(*group_idx) {
                    let s = m.as_str();
                    if s.is_empty() {
                        continue;
                    }
                    match slot {
                        Slot::Literal => literal = Some(s.to_string()),
                        Slot::Identifier => identifier = Some(s.to_string()),
                    }
                }
            }
            out.push(IdiomMatch {
                framework_id: idiom.id.clone(),
                kind: idiom.kind,
                literal,
                identifier,
                line: line_no,
            });
        }
    }
    out
}

/// Build a `Vec<CompiledIdiom>` from a YAML-framework iterator.
/// Entries whose `path_regex` is missing or fails to compile are
/// silently dropped (the YAML schema says `path_regex` is the
/// idioms' "the walker has something to match against" field —
/// missing it is the same as the entry being a stub). Tests
/// surface malformed entries via the `Patterns::framework` /
/// `Patterns::route_patterns_map` paths.
pub fn compile_idioms<'a, I>(iter: I) -> Vec<CompiledIdiom>
where
    I: IntoIterator<Item = &'a crate::server::sensors::patterns::FrameworkDef>,
{
    iter.into_iter()
        .filter_map(CompiledIdiom::from_def)
        .collect()
}

/// Build the full per-`Lang` idiom set in one shot: every
/// Tier-1 entry in the registry, partitioned by the call site
/// (the caller iterates lines and calls [`walk_idioms`] with
/// each sub-list). The signature returns the four sub-lists
/// `(producers, consumers, scheduled, …)` so the caller can
/// decide which to dispatch in which language walker without
/// re-querying `Patterns`.
#[allow(clippy::type_complexity)]
pub fn topic_idioms_for(
    patterns: &Patterns,
    lang: Lang,
) -> (Vec<CompiledIdiom>, Vec<CompiledIdiom>, Vec<CompiledIdiom>) {
    (
        compile_idioms(patterns.topic_producer_patterns(lang)),
        compile_idioms(patterns.topic_consumer_patterns(lang)),
        compile_idioms(patterns.scheduled_patterns(lang)),
    )
}

/// Build the WebSocket idiom set from a `Patterns` instance.
/// The pre-Tier-1 `websocket_sensor.rs` applied the same four
/// regexes to every source line regardless of `Lang`; the
/// migration keeps the same shape — the walker consumes the
/// `*_all` accessors and applies them to every file. A future
/// refinement that splits WebSocket detection by `Lang` (so
/// `wss?://…` in a Go file is filtered out, say) can swap
/// `*_all` for `*(lang)` here without touching the walker.
pub fn websocket_idioms_for(
    patterns: &Patterns,
) -> (Vec<CompiledIdiom>, Vec<CompiledIdiom>, Vec<CompiledIdiom>) {
    (
        compile_idioms(patterns.websocket_client_patterns_all()),
        compile_idioms(patterns.websocket_server_patterns_all()),
        compile_idioms(patterns.websocket_handler_patterns_all()),
    )
}

/// Bundle the parameters for [`emit_graphql_field_refs`].
pub struct GraphqlFieldRefs<'a> {
    pub consumer_id: &'a str,
    pub op: GraphqlOp,
    pub field: &'a str,
    pub site_path: &'a str,
    pub site_line: u32,
    pub selected_fields: &'a [String],
    pub namespace: &'a RepoNamespace,
}

/// Emit one `FieldRef` node per selected GraphQL field, plus
/// `ReadsFrom` and `ReadsField` edges linking it to the consumer
/// node. The id naming scheme (`graphql-read:<op>:<field>:<sel>`)
/// and the `FieldReadOrigin::GraphqlConsumer` origin are part of the
/// graph contract — joiner dispatch and `tests/graphql_resolution.rs`
/// match on them.
pub fn emit_graphql_field_refs(
    spec: GraphqlFieldRefs<'_>,
    all_nodes: &mut Vec<GraphNode>,
    all_edges: &mut Vec<GraphEdge>,
) {
    let GraphqlFieldRefs {
        consumer_id,
        op,
        field,
        site_path,
        site_line,
        selected_fields,
        namespace,
    } = spec;
    for (idx, sel) in selected_fields.iter().enumerate() {
        let ref_id_name = format!("graphql-read:{}:{}:{}", op, field, sel);
        let ref_id = GraphNode::generate_id(
            &NodeType::FieldRef,
            site_path,
            &ref_id_name,
            Some(site_line + idx as u32),
            namespace,
        );
        let mut ref_node = GraphNode::new(NodeType::FieldRef, sel.clone(), site_path.to_string());
        ref_node.id = ref_id.clone();
        ref_node.line_start = Some(site_line + idx as u32);
        ref_node.line_end = Some(site_line + idx as u32);
        ref_node.contract = vec![ContractFact::FieldRead(FieldReadFact {
            chain: JsonPath(vec![PathSegment::Name(sel.clone())]),
            exact: true,
            origin: FieldReadOrigin::GraphqlConsumer,
        })];
        all_nodes.push(ref_node);
        all_edges.push(GraphEdge::new(
            EdgeType::ReadsFrom,
            ref_id.clone(),
            consumer_id.to_string(),
        ));
        all_edges.push(GraphEdge::new(
            EdgeType::ReadsField,
            consumer_id.to_string(),
            ref_id,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{GraphNode, NodeType, RepoNamespace};

    fn db(_name: &str) -> GraphDatabase {
        // `tempfile::tempdir()` gives each test its own directory
        // (cleaned up on Drop), so concurrent test processes don't
        // race over a fixed `/tmp/util_…` path. The other walker
        // tests in this file already use this style. The
        // `GraphDatabase` opens an in-file index inside the temp
        // dir; the dir itself is the file's `memory_path`.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("graph.bin");
        GraphDatabase::new(&path).unwrap()
    }

    fn fn_with_range(name: &str, path: &str, start: u32, end: u32) -> GraphNode {
        let ns = RepoNamespace::for_test();
        GraphNode::new_in(NodeType::Function, name.to_string(), path.to_string(), &ns)
            .with_location_in(start, end, &ns)
    }

    #[test]
    fn snake_case_handles_camel_and_pascal() {
        assert_eq!(to_snake_case("getUser"), "get_user");
        assert_eq!(to_snake_case("GetUser"), "get_user");
        assert_eq!(to_snake_case("URL"), "u_r_l");
        assert_eq!(to_snake_case(""), "");
    }

    #[test]
    fn camel_case_handles_snake_and_passthrough() {
        assert_eq!(to_camel_case("get_user"), "getUser");
        assert_eq!(to_camel_case("GetUser"), "GetUser");
        assert_eq!(to_camel_case(""), "");
    }

    #[test]
    fn walker_visits_tracked_files_that_gitignore_matches() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("gen")).unwrap();
        std::fs::write(root.join(".gitignore"), "gen/\n").unwrap();
        std::fs::write(root.join("gen/routes.py"), "x = 1\n").unwrap();
        std::fs::write(root.join("gen/untracked.py"), "y = 1\n").unwrap();
        let repo = git2::Repository::init(root).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("gen/routes.py")).unwrap();
        index.write().unwrap();
        let names: Vec<_> = walk_workspace(root)
            .filter_map(|e| e.path().file_name().map(|n| n.to_owned()))
            .collect();
        assert!(names.iter().any(|n| n == "routes.py"), "{names:?}");
        assert!(!names.iter().any(|n| n == "untracked.py"), "{names:?}");
    }

    #[test]
    fn walker_visits_a_written_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.proto"), "syntax = \"proto3\";\n").unwrap();
        let names: Vec<_> = walk_workspace(dir.path())
            .map(|e| e.path().to_path_buf())
            .filter_map(|p| p.file_name().map(|n| n.to_owned()))
            .collect();
        assert!(
            names.iter().any(|n| n == "a.proto"),
            "walker missed the file: {names:?}"
        );
    }

    /// `enclosing_symbol` returns the smallest range covering the
    /// line. A 50-line nested function must beat a 200-line
    /// surrounding one for a line inside both — otherwise the
    /// joiner (PR 7) would attach every `SendsHttp` to the
    /// outermost symbol.
    #[test]
    fn enclosing_symbol_returns_the_smallest_range() {
        let g = db("enclosing_smallest");
        let outer = fn_with_range("outer", "src/x.py", 1, 200);
        let inner = fn_with_range("inner", "src/x.py", 50, 100);
        g.insert_nodes_batch(&[outer.clone(), inner.clone()])
            .unwrap();

        let found = enclosing_symbol(&g, "src/x.py", 75).expect("a symbol covers line 75");
        assert_eq!(found.name, "inner", "smallest range wins");
        assert_eq!(found.id, inner.id);
    }

    /// When two symbols span exactly the same number of lines, the
    /// one whose `line_start` is later wins — a nested helper
    /// declared further down the file is more specific than the
    /// outer block that starts at the top.
    #[test]
    fn enclosing_symbol_breaks_range_ties_by_later_line_start() {
        let g = db("enclosing_tie");
        // Both ranges span 100 lines; B's line_start (50) is later
        // than A's (1), so B wins on line 75.
        let a = fn_with_range("a", "src/x.py", 1, 101);
        let b = fn_with_range("b", "src/x.py", 50, 150);
        g.insert_nodes_batch(&[a.clone(), b.clone()]).unwrap();

        let found = enclosing_symbol(&g, "src/x.py", 75).expect("a symbol covers line 75");
        assert_eq!(found.name, "b", "tie → later line_start");
        assert_eq!(found.id, b.id);
    }

    /// A `File` node whose own range covers the line must not be
    /// returned, even when no `Function`/`Method` covers it. The
    /// doc contract is "no candidate → None"; the only way that
    /// path triggers in practice is when the candidate set has
    /// nothing but `File` (or other non-`Function`/`Method`)
    /// nodes, because the resolver only attaches `SendsHttp` and
    /// `Calls` to enclosing functions/methods. A regression that
    /// widened the candidate set to include `File` would return
    /// the file node here and fail.
    #[test]
    fn enclosing_symbol_returns_none_when_only_a_file_covers_the_line() {
        let g = db("enclosing_file_only");
        let ns = RepoNamespace::for_test();
        // A file-shaped node covering the whole file.
        let file = GraphNode::new_in(
            NodeType::File,
            "x.py".to_string(),
            "src/x.py".to_string(),
            &ns,
        )
        .with_location_in(1, 200, &ns);
        g.insert_nodes_batch(&[file]).unwrap();

        // No Function/Method covers line 50 — only the File node
        // does. The type filter must drop it; without the filter,
        // the function under test would return the File.
        assert!(
            enclosing_symbol(&g, "src/x.py", 50).is_none(),
            "a File node covering the line is not a Function/Method; the type filter must drop it"
        );
    }

    /// Variant of the same rule for a file-shape node that is the
    /// *smallest* covering range. Without the type filter, smallest-
    /// range wins and the File would be returned — this test pins
    /// the filter as load-bearing even when range selection would
    /// otherwise pick the File.
    #[test]
    fn enclosing_symbol_excludes_a_file_with_the_smallest_covering_range() {
        let g = db("enclosing_file_smallest");
        let ns = RepoNamespace::for_test();
        // File covers lines 1-3 (smallest possible range); the
        // surrounding Function covers 1-200.
        let file = GraphNode::new_in(
            NodeType::File,
            "x.py".to_string(),
            "src/x.py".to_string(),
            &ns,
        )
        .with_location_in(1, 3, &ns);
        let func = fn_with_range("the_function", "src/x.py", 1, 200);
        g.insert_nodes_batch(&[file, func.clone()]).unwrap();

        let found = enclosing_symbol(&g, "src/x.py", 2)
            .expect("a Function covers line 2 even though File does too");
        assert_eq!(found.node_type, NodeType::Function);
        assert_eq!(found.name, "the_function");
        // A regression that loosened the candidate set to include
        // File would return the file node — smallest range wins
        // before the type filter — and the assertion above would
        // fail on `found.node_type == NodeType::File`.
    }

    /// A `Method` node is also accepted — both `Function` and
    /// `Method` are in the candidate set per §6.1.
    #[test]
    fn enclosing_symbol_accepts_methods() {
        let g = db("enclosing_method");
        let ns = RepoNamespace::for_test();
        let method = GraphNode::new_in(
            NodeType::Method,
            "the_method".to_string(),
            "src/x.rs".to_string(),
            &ns,
        )
        .with_location_in(10, 20, &ns);
        g.insert_nodes_batch(std::slice::from_ref(&method)).unwrap();

        let found = enclosing_symbol(&g, "src/x.rs", 15).expect("a method covers line 15");
        assert_eq!(found.name, "the_method");
        assert_eq!(found.node_type, NodeType::Method);
    }

    /// A line outside every range returns `None` — the sensor
    /// shouldn't claim an enclosing symbol for, say, a top-of-file
    /// import.
    #[test]
    fn enclosing_symbol_returns_none_when_no_range_covers_the_line() {
        let g = db("enclosing_no_match");
        let f = fn_with_range("the_function", "src/x.py", 50, 100);
        g.insert_nodes_batch(&[f]).unwrap();

        assert!(
            enclosing_symbol(&g, "src/x.py", 10).is_none(),
            "line 10 is before the function"
        );
        assert!(
            enclosing_symbol(&g, "src/x.py", 200).is_none(),
            "line 200 is after the function"
        );
        assert!(
            enclosing_symbol(&g, "src/other.py", 75).is_none(),
            "path that doesn't exist in the graph returns None"
        );
    }
}
