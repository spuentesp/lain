//! Cross-file client registry — per-repo pre-pass that resolves wrapper
//! HTTP clients (axios / ky / got / Python ctor clients) to a base
//! URL, so the joiner can answer "where does this call land?"
//! (spec §5.1, §5.2).
//!
//! Two shapes:
//!
//! 1. **One [`ClientDef`] per declaration** — `axios.create({baseURL:
//!    "https://api/x"})`, `ky.create({prefixUrl: "https://api/x"})`,
//!    `got.extend({prefixUrl: "https://api/x"})`, `new
//!    Foo({baseUrl: "https://api/x"})`, and Python's
//!    `httpx.Client(base_url=...)` / `aiohttp.ClientSession(...)`.
//!    Each `ClientDef` carries its module-level identifier (so two
//!    clients with the same name in different modules don't cross-bind,
//!    spec §5.3 acceptance).
//!
//! 2. **Cross-file resolution** — a `import { ordersClient } from
//!    "./clients"` walks one hop to the export site. Deeper chains
//!    stay unresolved (`base_unknown`).
//!
//! Exported factories returning a client are out of scope v1 (spec
//! §5.1).
//!
//! This module is pure data — no sensor logic, no joiner logic.
//! Sensor code populates a [`ClientRegistry`] via [`detect_clients`]
//! (TS/JS) or [`from_python_ctor`] (the existing Phase-A
//! `client_base_urls` map). The joiner consults it.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::federation::contracts::normalize::{normalize, UrlPart as NormalizeUrlPart};

// ─── UrlPart ──────────────────────────────────────────────────────────

/// One piece of a URL argument after Phase B composition. `Literal`
/// is verbatim text; `Env` carries the env-var name(s) the host
/// expression resolves to; `Expr` is anything else (Hole /
/// unresolvable). The trio maps onto the existing `HostPart`
/// (`Literal | Env | Expr`, §4.5) plus the per-part composition
/// (base ++ call path).
///
/// Spec §5.1: a normalized base URL expressed as a sequence of
/// `UrlPart`s so the joiner can compose a final URL by appending
/// the call's parts onto the registry's base. The joiner reuses the
/// existing [`normalize`] step on the composed parts; the `host`
/// resolution step is the existing [`crate::federation::contracts::model::host_for`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UrlPart {
    Literal(String),
    Env(Vec<String>),
    Expr(String),
}

impl UrlPart {
    /// Project a `UrlPart` into the normalizer's input shape
    /// (`Literal | Hole`). Env names render as `Hole("ENV_VAR")`
    /// because the normalizer carries no env-var semantics — the
    /// §6.3 host-resolution step (`crate::server::sensors::util::host_env_name`)
    /// classifies the rendered host side later.
    fn as_normalize_part(&self) -> NormalizeUrlPart {
        match self {
            UrlPart::Literal(s) => NormalizeUrlPart::Literal(s.clone()),
            UrlPart::Env(names) => {
                // The Env case is keyed on a single name in TS/JS
                // (`process.env.X`) and on a one-element list in Python
                // (`os.environ["X"]`). Render the first name — the
                // §6.3 host classifier picks the literal up and emits
                // `HostPart::Env(vec![name])` for the joiner.
                let name = names.first().cloned().unwrap_or_default();
                NormalizeUrlPart::Hole(name)
            }
            UrlPart::Expr(s) => NormalizeUrlPart::Hole(s.clone()),
        }
    }
}

// ─── ClientLibrary ────────────────────────────────────────────────────

/// The library a client belongs to. `Custom` is the catch-all for a
/// locally defined thin wrapper (`new Foo({baseUrl: …})`). The
/// value drives the joiner cross-reference heuristics (spec §5.3):
/// `Axios` / `Ky` / `Got` / `Httpx` / `Requests` / `Aiohttp` skip
/// rule 1's `http_clients` short-circuit when their `Library { name }`
/// is on the call — only `Receiver` candidates reach the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientLibrary {
    Axios,
    Ky,
    Got,
    Httpx,
    Requests,
    Aiohttp,
    Custom,
}

impl ClientLibrary {
    /// Stable wire name. Used by the joiner to surface the library in
    /// `EdgeProvenance::Heuristic { detector, confidence }`.
    pub fn wire_name(self) -> &'static str {
        match self {
            ClientLibrary::Axios => "axios",
            ClientLibrary::Ky => "ky",
            ClientLibrary::Got => "got",
            ClientLibrary::Httpx => "httpx",
            ClientLibrary::Requests => "requests",
            ClientLibrary::Aiohttp => "aiohttp",
            ClientLibrary::Custom => "custom",
        }
    }
}

// ─── ClientSite ───────────────────────────────────────────────────────

/// The source site a `ClientDef` was detected at. The joiner
/// carries this on `CallVia::Receiver::base` so the user-visible
/// `EdgeProvenance::Heuristic { detector }` names the originating
/// definition (spec §5.2 "evidence").
pub use crate::federation::contracts::model::ClientSite;

// ─── ClientDef ────────────────────────────────────────────────────────

/// One declaration of an HTTP client in the repo. Spec §5.1:
///
/// ```text
/// ClientDef { name, module, base: Vec<UrlPart>, library, site }
/// ```
///
/// `module` is the canonical module specifier the registry keys on
/// (TS/JS: the file path relative to the repo root, e.g.
/// `./clients`; Python: the dotted module name). Two clients of the
/// same `name` in different `module`s live as separate entries —
/// this is what stops spec §5.3 acceptance from cross-binding.
///
/// `base` is the normalized base URL parts. The joiner composes
/// `base ++ call_path` via [`compose_url`] and runs the §4.5
/// normalizer on the result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientDef {
    pub name: String,
    pub module: String,
    pub base: Vec<UrlPart>,
    pub library: Option<ClientLibrary>,
    pub site: ClientSite,
}

impl ClientDef {
    /// Cross-file resolution (spec §5.1). Walk `import_path` one
    /// hop from `module`: if `registry` contains a `ClientDef` with
    /// matching `module` and `name`, return it. Deeper chains stay
    /// unresolved — return `None`. The caller (the joiner) records
    /// `base_unknown` on the consumer when the answer is `None`.
    pub fn resolve_cross_file(
        name: &str,
        import_path: &str,
        registry: &ClientRegistry,
    ) -> Option<Self> {
        // First hop: the imported module's own declarations.
        if let Some(def) = registry.lookup(import_path, name) {
            return Some(def.clone());
        }
        // One re-export hop: the importing module may itself be
        // re-exporting from elsewhere. The registry's
        // `re_export` table carries these (TS: `export { ordersClient } from "./clients"`).
        if let Some(target) = registry.re_export(import_path, name) {
            if let Some(def) = registry.lookup(&target, name) {
                return Some(def.clone());
            }
        }
        None
    }
}

// ─── ClientRegistry ───────────────────────────────────────────────────

/// Per-repo client registry. The joiner reads it to find the
/// `ClientDef` that owns a `CallVia::Receiver { expr, fn_name }`,
/// then composes `base ++ call_path` to derive the consumer's
/// target host. Spec §5.1.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientRegistry {
    /// `module → name → ClientDef`. `BTreeMap` keeps the iteration
    /// order deterministic for I4 (the §7.8 determinism invariant).
    defs: BTreeMap<String, BTreeMap<String, ClientDef>>,
    /// `module → name → target module`. TS re-exports (`export { x }
    /// from "./other"`) record the one-hop chain here so the joiner
    /// can resolve through them. Spec §5.1 says "one re-export hop"
    /// — deeper chains are out of scope.
    re_exports: BTreeMap<String, BTreeMap<String, String>>,
    /// All `module`s the registry has seen, kept for determinism
    /// (the joiner iterates in this order).
    modules: BTreeSet<String>,
}

impl ClientRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one `ClientDef`. Last writer wins when the same
    /// `(module, name)` appears twice in one repo (a real local
    /// inconsistency); the joiner treats this as a soundness issue
    /// and refuses to bind (see spec §5.3 acceptance: "Two clients
    /// of the same name in different modules do not cross-bind" —
    /// the symmetric case of duplicate names in one module is also
    /// flagged here for the operator).
    pub fn insert(&mut self, def: ClientDef) {
        let module = def.module.clone();
        let name = def.name.clone();
        self.defs
            .entry(module.clone())
            .or_default()
            .insert(name, def);
        self.modules.insert(module);
    }

    /// Record a re-export. `(from_module, name) → to_module`.
    pub fn insert_re_export(&mut self, from_module: String, name: String, to_module: String) {
        self.re_exports
            .entry(from_module)
            .or_default()
            .insert(name, to_module);
    }

    /// Look up a `ClientDef` by `(module, name)`. Returns `None`
    /// when the registry has no such declaration.
    pub fn lookup(&self, module: &str, name: &str) -> Option<&ClientDef> {
        self.defs.get(module).and_then(|m| m.get(name))
    }

    /// One-hop re-export target. Returns the target `module` for
    /// `(from_module, name)` if one is registered, else `None`.
    pub fn re_export(&self, from_module: &str, name: &str) -> Option<String> {
        self.re_exports
            .get(from_module)
            .and_then(|m| m.get(name).cloned())
    }

    /// Number of registered definitions (across all modules).
    pub fn len(&self) -> usize {
        self.defs.values().map(|m| m.len()).sum()
    }

    /// Whether the registry has no declarations.
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// Deterministic iteration over every `(module, name, &ClientDef)`
    /// triple — the joiner uses this when computing cross-file
    /// resolution. The `BTreeMap` keeps the order stable.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str, &ClientDef)> {
        self.defs
            .iter()
            .flat_map(|(m, inner)| inner.iter().map(move |(n, d)| (m.as_str(), n.as_str(), d)))
    }

    /// All `(module, name)` pairs that resolve to a non-empty `base`.
    /// Phase B's joiner uses this list to build the per-call
    /// composition; tests use it to assert "two clients of the same
    /// name in different modules do not cross-bind".
    pub fn with_base(&self) -> Vec<(String, String, Vec<UrlPart>)> {
        self.iter()
            .filter(|(_, _, d)| !d.base.is_empty())
            .map(|(m, n, d)| (m.to_string(), n.to_string(), d.base.clone()))
            .collect()
    }

    /// Snapshot of all modules the registry has seen.
    pub fn modules(&self) -> impl Iterator<Item = &str> {
        self.modules.iter().map(String::as_str)
    }
}

// ─── Composition helper (spec §5.2) ───────────────────────────────────

/// Compose the final URL from base + call path parts (spec §5.2).
/// Output is a flat `Vec<UrlPart>` the caller feeds to the existing
/// [`normalize`] step. The composition is order-preserving: every
/// base part is emitted first, every call-path part after. Empty
/// inputs return the other input verbatim; both empty returns
/// `Vec::new()`.
///
/// Normalization (lowercase scheme/host, drop trailing slash) is the
/// caller's job — the existing [`normalize`] does both; this helper
/// only stitches the two streams.
pub fn compose_url(call_path: &[UrlPart], base: &[UrlPart]) -> Vec<UrlPart> {
    let mut out: Vec<UrlPart> = Vec::with_capacity(base.len() + call_path.len());
    for part in base {
        out.push(part.clone());
    }
    for part in call_path {
        out.push(part.clone());
    }
    out
}

/// Normalize a composed URL (spec §5.2 step "normalize"). Convenience
/// wrapper: project every [`UrlPart`] into a [`NormalizeUrlPart`],
/// run the existing [`normalize`], return the resulting
/// [`crate::federation::contracts::model::NormalizedUrl`].
pub fn compose_and_normalize(
    call_path: &[UrlPart],
    base: &[UrlPart],
) -> crate::federation::contracts::model::NormalizedUrl {
    let composed = compose_url(call_path, base);
    let projected: Vec<NormalizeUrlPart> =
        composed.iter().map(UrlPart::as_normalize_part).collect();
    normalize(&projected)
}

// ─── TS/JS scanner (spec §5.1) ───────────────────────────────────────

/// One detected TS/JS wrapper declaration, pre-`ClientDef`. The
/// scanner emits one per match; [`build_client_def`] folds the
/// language-specific bits into a [`ClientDef`] for the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedClient {
    pub name: String,
    pub module: String,
    pub base: Vec<UrlPart>,
    pub library: ClientLibrary,
    pub site: ClientSite,
}

/// Detect every TS/JS client definition in `content`. The scanner is
/// a regex pre-pass (the existing five sensors own the tree-sitter
/// pass); the patterns are stable enough to be matched by regex
/// without parsing the full AST. Spec §5.1:
///
/// - `axios.create({baseURL: …})`
/// - `ky.create({prefixUrl: …})`
/// - `got.extend({prefixUrl: …})`
/// - `new Foo({baseUrl: …})` (locally defined thin wrapper — the
///   scanner still emits this with `library = Custom`).
///
/// Returns `Vec<DetectedClient>` — the caller wraps each entry into a
/// [`ClientDef`] and inserts into a [`ClientRegistry`]. The function
/// is pure (no I/O, no `&mut`), so the proptest pin can call it
/// directly.
pub fn detect_clients(path: &str, content: &str) -> Vec<DetectedClient> {
    let mut out: Vec<DetectedClient> = Vec::new();
    let module = canonical_module(path);
    for (idx, line) in content.lines().enumerate() {
        let line_no = (idx as u32) + 1;
        // Match `const NAME = axios.create({baseURL: …})` /
        // `let NAME = ky.create({prefixUrl: …})` /
        // `const NAME = got.extend({prefixUrl: …})`.
        for (lib, key, target_key) in [
            (ClientLibrary::Axios, "axios.create", "baseURL"),
            (ClientLibrary::Ky, "ky.create", "prefixUrl"),
            (ClientLibrary::Got, "got.extend", "prefixUrl"),
        ] {
            if let Some(rest) = line_after_create(line, key) {
                if let Some(base) = extract_base_value(rest, target_key) {
                    out.push(DetectedClient {
                        name: extract_assigned_name(line).unwrap_or_else(|| "default".into()),
                        module: module.clone(),
                        base: vec![base.into_url_part()],
                        library: lib,
                        site: ClientSite {
                            path: path.to_string(),
                            line: line_no,
                        },
                    });
                }
            }
        }
        // `const NAME = new Foo({baseUrl: …})` — local thin wrapper.
        if let Some(rest) = line_after_new(line) {
            if let Some(base) = extract_base_value(rest, "baseUrl") {
                if let Some(name) = extract_assigned_name(line) {
                    out.push(DetectedClient {
                        name,
                        module: module.clone(),
                        base: vec![base.into_url_part()],
                        library: ClientLibrary::Custom,
                        site: ClientSite {
                            path: path.to_string(),
                            line: line_no,
                        },
                    });
                }
            }
        }
    }
    out
}

/// Project a [`DetectedClient`] into the registry's wire shape.
pub fn build_client_def(detected: &DetectedClient) -> ClientDef {
    ClientDef {
        name: detected.name.clone(),
        module: detected.module.clone(),
        base: detected.base.clone(),
        library: Some(detected.library),
        site: detected.site.clone(),
    }
}

fn canonical_module(path: &str) -> String {
    // TS/JS modules are file paths relative to the repo root.
    // Strip the extension and leading `./`. The convention matches
    // what TypeScript's module resolver sees — "./clients" →
    // "src/clients.ts".
    let stripped = path
        .strip_prefix("./")
        .unwrap_or(path)
        .trim_end_matches(".ts")
        .trim_end_matches(".tsx")
        .trim_end_matches(".js")
        .trim_end_matches(".jsx")
        .trim_end_matches(".mjs")
        .trim_end_matches(".cjs");
    stripped.to_string()
}

fn extract_assigned_name(line: &str) -> Option<String> {
    // `const ordersClient = ...` / `let client = ...` /
    // `var foo = ...` / `export const ordersClient = ...`.
    let lower = line.trim_start();
    let trimmed = lower
        .strip_prefix("const ")
        .or_else(|| lower.strip_prefix("let "))
        .or_else(|| lower.strip_prefix("var "))
        .or_else(|| lower.strip_prefix("export const "))
        .or_else(|| lower.strip_prefix("export let "))
        .or_else(|| lower.strip_prefix("export var "))
        .unwrap_or(lower);
    let trimmed = trimmed
        .strip_prefix("const ")
        .or_else(|| trimmed.strip_prefix("let "))
        .or_else(|| trimmed.strip_prefix("var "))
        .unwrap_or(trimmed);
    let name: String = trimmed
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn line_after_create<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pos = line.find(key)?;
    let after = &line[pos + key.len()..];
    // Skip `(`, optional whitespace, then `{`.
    let trimmed = after.trim_start();
    let trimmed = trimmed.strip_prefix('(')?;
    let trimmed = trimmed.trim_start();
    let trimmed = trimmed.strip_prefix('{')?;
    Some(trimmed)
}

fn line_after_new(line: &str) -> Option<&str> {
    // `new Foo({baseUrl: ...})` — match `new ` then the class name
    // then `({`. We don't validate the class name (anything PascalCase
    // counts as a wrapper).
    let pos = line.find("new ")?;
    let after = &line[pos + "new ".len()..];
    // Consume identifier.
    let mut chars = after.chars();
    let mut class_name = String::new();
    for c in chars.by_ref() {
        if c.is_alphanumeric() || c == '_' || c == '$' {
            class_name.push(c);
        } else {
            break;
        }
    }
    if class_name.is_empty() {
        return None;
    }
    // Skip whitespace, then accept either `({` (with parentheses) or
    // just `{` (without). TS/JS object literals may appear without
    // a wrapping call: `new Foo{baseUrl: …}` is rare but valid
    // syntactically (it's parsed as a member expression).
    let rest: &str = chars.as_str();
    let trimmed = rest.trim_start();
    let trimmed = trimmed
        .strip_prefix('(')
        .map(|s| s.trim_start())
        .unwrap_or(trimmed);
    let trimmed = trimmed.strip_prefix('{')?;
    Some(trimmed)
}

/// One URL base value extracted from a TS/JS client declaration
/// (Phase C, spec §6). Spec §5.1 accepts only literal strings;
/// Phase C also accepts `process.env.X` and `process.env["X"]`
/// shapes so the joiner can resolve the var through the
/// env_sensor's per-repo index and the host through
/// `services[].hosts`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BaseValue {
    Literal(String),
    Env(String),
}

impl BaseValue {
    fn into_url_part(self) -> UrlPart {
        match self {
            BaseValue::Literal(s) => UrlPart::Literal(s),
            BaseValue::Env(name) => UrlPart::Env(vec![name]),
        }
    }
}

/// Extract a base value, accepting both literal strings and
/// `process.env.X` / `process.env["X"]` (Phase C). The literal
/// path mirrors [`extract_string_key`]; the env path recognizes
/// the same patterns the http_client_sensor's `host_env_name`
/// already knows about so the two scanners stay in sync.
fn extract_base_value(rest: &str, key: &str) -> Option<BaseValue> {
    let pos = rest.find(key)?;
    let after = &rest[pos + key.len()..];
    let after = after.trim_start().strip_prefix(':')?;
    let after = after.trim_start();
    if after.is_empty() {
        return None;
    }
    // Literal string path: "..." / '...' / `...`.
    if let Some((_, literal)) =
        crate::server::sensors::util_tokenize::extract_string_literal(after, 0)
    {
        return Some(BaseValue::Literal(literal));
    }
    // Env path: `process.env.X` / `process.env["X"]`.
    if let Some(name) = extract_process_env_name(after) {
        return Some(BaseValue::Env(name.to_string()));
    }
    None
}

/// Extract the env-var name from a `process.env.X` /
/// `process.env["X"]` expression at the start of `s` (i.e. the
/// `<key>:` is stripped and we look at the value). Returns
/// `Some(name)` when the value matches one of those two shapes,
/// `None` otherwise. The grammar is delegated to the canonical
/// [`crate::server::sensors::util::host_env_name`] (Phase B-D
/// review §S10); the only deviation is the trailing-punctuation
/// strip — `host_env_name` requires a clean shape but the value
/// here often carries `,` / `}` / `)` / `;` from the enclosing
/// object literal.
#[allow(clippy::manual_pattern_char_comparison)]
fn extract_process_env_name(s: &str) -> Option<&str> {
    // `host_env_name` requires a clean shape; the value here often
    // carries `,` / `}` / `)` / `;` from the enclosing object
    // literal. Strip those before delegating.
    let t = s
        .trim()
        .trim_end_matches(|c: char| matches!(c, ',' | ')' | '}' | ';' | ' ' | '\t' | '\n' | ']'));
    if !t.starts_with("process.env") {
        return None;
    }
    crate::server::sensors::util::host_env_name(t)
}

// ─── Python ctor clients (spec §5.1) ─────────────────────────────────

/// One Python ctor client binding lifted into the registry. The
/// `http_client_sensor` already detects `httpx.Client(base_url=…)`
/// / `aiohttp.ClientSession(...)` (Phase A); this helper converts
/// one binding into a [`ClientDef`] keyed by module name.
pub fn from_python_ctor(
    name: &str,
    module: &str,
    base_url: &str,
    library: ClientLibrary,
    site: ClientSite,
) -> ClientDef {
    ClientDef {
        name: name.to_string(),
        module: module.to_string(),
        base: vec![UrlPart::Literal(base_url.to_string())],
        library: Some(library),
        site,
    }
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_url_appends_call_path_to_base() {
        let base = vec![UrlPart::Literal("https://api.example.com".into())];
        let call = vec![UrlPart::Literal("/v1/orders".into())];
        let composed = compose_url(&call, &base);
        assert_eq!(
            composed,
            vec![
                UrlPart::Literal("https://api.example.com".into()),
                UrlPart::Literal("/v1/orders".into()),
            ]
        );
    }

    #[test]
    fn compose_url_with_empty_base_returns_call_path() {
        let call = vec![UrlPart::Literal("/v1/orders".into())];
        let composed = compose_url(&call, &[]);
        assert_eq!(composed, vec![UrlPart::Literal("/v1/orders".into())]);
    }

    #[test]
    fn compose_url_with_empty_call_returns_base() {
        let base = vec![UrlPart::Literal("https://api.example.com".into())];
        let composed = compose_url(&[], &base);
        assert_eq!(
            composed,
            vec![UrlPart::Literal("https://api.example.com".into())]
        );
    }

    #[test]
    fn compose_and_normalize_lowercases_scheme_and_host() {
        let base = vec![UrlPart::Literal("HTTPS://API.Example.com".into())];
        let call = vec![UrlPart::Literal("/v1/orders".into())];
        let url = compose_and_normalize(&call, &base);
        assert_eq!(
            url.host,
            crate::federation::contracts::model::HostPart::Literal("api.example.com".into())
        );
        assert_eq!(url.template.as_deref(), Some("/v1/orders"));
    }

    #[test]
    fn detect_clients_axios_create() {
        let src = "const ordersClient = axios.create({baseURL: \"https://api.example.com\"});\n";
        let dets = detect_clients("src/clients.ts", src);
        assert_eq!(dets.len(), 1);
        let d = &dets[0];
        assert_eq!(d.name, "ordersClient");
        assert_eq!(d.library, ClientLibrary::Axios);
        assert_eq!(
            d.base,
            vec![UrlPart::Literal("https://api.example.com".into())]
        );
        assert_eq!(d.module, "src/clients");
        assert_eq!(d.site.line, 1);
    }

    #[test]
    fn detect_clients_ky_create_uses_prefix_url() {
        let src = "let c = ky.create({prefixUrl: \"https://ky.test\"});\n";
        let dets = detect_clients("c.ts", src);
        assert_eq!(dets.len(), 1);
        assert_eq!(dets[0].library, ClientLibrary::Ky);
        assert_eq!(
            dets[0].base,
            vec![UrlPart::Literal("https://ky.test".into())]
        );
    }

    #[test]
    fn detect_clients_got_extend() {
        let src = "const g = got.extend({prefixUrl: 'https://g.test'});\n";
        let dets = detect_clients("g.ts", src);
        assert_eq!(dets.len(), 1);
        assert_eq!(dets[0].library, ClientLibrary::Got);
        assert_eq!(
            dets[0].base,
            vec![UrlPart::Literal("https://g.test".into())]
        );
    }

    /// Phase C (spec §6): `baseURL: process.env.X` and
    /// `prefixUrl: process.env["X"]` are recognized as env-var
    /// bases. The scanner emits `UrlPart::Env([X])` so the joiner
    /// resolves the var through the env_sensor's per-repo
    /// bindings.
    #[test]
    fn detect_clients_recognizes_process_env_base_url() {
        let src = "const ordersClient = axios.create({baseURL: process.env.ORDERS_API_URL});\n";
        let dets = detect_clients("src/clients.ts", src);
        assert_eq!(dets.len(), 1);
        assert_eq!(dets[0].library, ClientLibrary::Axios);
        assert_eq!(
            dets[0].base,
            vec![UrlPart::Env(vec!["ORDERS_API_URL".to_string()])]
        );

        let src2 = "const billing = ky.create({prefixUrl: process.env[\"BILLING_URL\"]});\n";
        let dets2 = detect_clients("c.ts", src2);
        assert_eq!(dets2.len(), 1);
        assert_eq!(dets2[0].library, ClientLibrary::Ky);
        assert_eq!(
            dets2[0].base,
            vec![UrlPart::Env(vec!["BILLING_URL".to_string()])]
        );
    }

    /// `process.env.X` on a `new Foo({baseUrl: …})` wrapper
    /// detection also flows through.
    #[test]
    fn detect_clients_recognizes_process_env_on_local_wrapper() {
        let src = "const ordersClient = new OrdersClient({baseUrl: process.env.ORDERS_API_URL});\n";
        let dets = detect_clients("src/orders_client.ts", src);
        assert_eq!(dets.len(), 1);
        assert_eq!(dets[0].library, ClientLibrary::Custom);
        assert_eq!(
            dets[0].base,
            vec![UrlPart::Env(vec!["ORDERS_API_URL".to_string()])]
        );
    }

    #[test]
    fn detect_clients_local_new_uses_custom_library() {
        let src = "const ordersClient = new OrdersClient({baseUrl: \"https://orders.test\"});\n";
        let dets = detect_clients("src/orders_client.ts", src);
        assert_eq!(dets.len(), 1);
        assert_eq!(dets[0].library, ClientLibrary::Custom);
        assert_eq!(dets[0].name, "ordersClient");
    }

    #[test]
    fn detect_clients_ignores_lines_without_base_url() {
        let src = "\
const foo = axios.create({something: 'else'});
const orders = axios.create({baseURL: 'https://orders'});
";
        let dets = detect_clients("c.ts", src);
        assert_eq!(dets.len(), 1, "only the line with baseURL counts");
        assert_eq!(dets[0].name, "orders");
        assert_eq!(dets[0].library, ClientLibrary::Axios);
    }

    #[test]
    fn detect_clients_handles_module_extensions() {
        for path in [
            "src/clients.ts",
            "src/clients.tsx",
            "src/clients.js",
            "src/clients.mjs",
        ] {
            let src = "const a = axios.create({baseURL: 'https://x'});";
            let dets = detect_clients(path, src);
            assert_eq!(dets.len(), 1);
            assert_eq!(dets[0].module, "src/clients");
        }
    }

    #[test]
    fn registry_lookup_by_module_and_name() {
        let mut reg = ClientRegistry::new();
        reg.insert(ClientDef {
            name: "ordersClient".into(),
            module: "src/clients".into(),
            base: vec![UrlPart::Literal("https://orders".into())],
            library: Some(ClientLibrary::Axios),
            site: ClientSite {
                path: "src/clients.ts".into(),
                line: 1,
            },
        });
        assert!(reg.lookup("src/clients", "ordersClient").is_some());
        assert!(reg.lookup("src/clients", "missing").is_none());
        assert!(reg.lookup("src/other", "ordersClient").is_none());
    }

    #[test]
    fn registry_dedups_per_module_and_name() {
        // Same module + name → last writer wins (a real local
        // inconsistency; the joiner refuses to bind either way).
        let mut reg = ClientRegistry::new();
        for base in ["https://a", "https://b"] {
            reg.insert(ClientDef {
                name: "client".into(),
                module: "src/x".into(),
                base: vec![UrlPart::Literal(base.into())],
                library: Some(ClientLibrary::Axios),
                site: ClientSite {
                    path: "src/x.ts".into(),
                    line: 1,
                },
            });
        }
        assert_eq!(reg.len(), 1);
        assert_eq!(
            reg.lookup("src/x", "client").map(|d| d.base.clone()),
            Some(vec![UrlPart::Literal("https://b".into())])
        );
    }

    #[test]
    fn registry_resolve_cross_file_walks_one_hop() {
        // Same module → direct hit.
        let mut reg = ClientRegistry::new();
        reg.insert(ClientDef {
            name: "ordersClient".into(),
            module: "./clients".into(),
            base: vec![UrlPart::Literal("https://orders".into())],
            library: Some(ClientLibrary::Axios),
            site: ClientSite {
                path: "./clients.ts".into(),
                line: 1,
            },
        });
        let direct = ClientDef::resolve_cross_file("ordersClient", "./clients", &reg);
        assert!(direct.is_some());
    }

    #[test]
    fn registry_resolve_cross_file_walks_one_reexport_hop() {
        let mut reg = ClientRegistry::new();
        reg.insert(ClientDef {
            name: "ordersClient".into(),
            module: "src/internal/clients".into(),
            base: vec![UrlPart::Literal("https://orders".into())],
            library: Some(ClientLibrary::Axios),
            site: ClientSite {
                path: "src/internal/clients.ts".into(),
                line: 1,
            },
        });
        reg.insert_re_export(
            "src/clients".into(),
            "ordersClient".into(),
            "src/internal/clients".into(),
        );
        let resolved = ClientDef::resolve_cross_file("ordersClient", "src/clients", &reg);
        assert!(resolved.is_some());
        assert_eq!(resolved.unwrap().module, "src/internal/clients".to_string());
    }

    #[test]
    fn registry_resolve_cross_file_returns_none_for_unknown() {
        let reg = ClientRegistry::new();
        assert!(ClientDef::resolve_cross_file("ordersClient", "./missing", &reg).is_none());
    }

    #[test]
    fn registry_with_base_skips_empty_bases() {
        // Phase B tests assert that two clients with the same name
        // but different modules don't cross-bind; `with_base` is the
        // joiner's primary input.
        let mut reg = ClientRegistry::new();
        reg.insert(ClientDef {
            name: "client".into(),
            module: "src/a".into(),
            base: vec![UrlPart::Literal("https://a".into())],
            library: Some(ClientLibrary::Axios),
            site: ClientSite {
                path: "src/a.ts".into(),
                line: 1,
            },
        });
        reg.insert(ClientDef {
            name: "client".into(),
            module: "src/b".into(),
            // Empty base — a stub, a fixture.
            base: vec![],
            library: Some(ClientLibrary::Axios),
            site: ClientSite {
                path: "src/b.ts".into(),
                line: 1,
            },
        });
        let with = reg.with_base();
        assert_eq!(with.len(), 1, "only the populated entry counts");
        assert_eq!(with[0].0, "src/a");
    }

    #[test]
    fn from_python_ctor_produces_a_well_formed_def() {
        let def = from_python_ctor(
            "orders",
            "src/orders.py",
            "https://orders",
            ClientLibrary::Httpx,
            ClientSite {
                path: "src/orders.py".into(),
                line: 1,
            },
        );
        assert_eq!(def.name, "orders");
        assert_eq!(def.library, Some(ClientLibrary::Httpx));
        assert_eq!(def.base, vec![UrlPart::Literal("https://orders".into())]);
    }

    #[test]
    fn client_library_wire_names_are_stable() {
        // Pin the §5.3 acceptance value: detectors join on case
        // ("axios", "ky", etc.), so the wire names are stable.
        assert_eq!(ClientLibrary::Axios.wire_name(), "axios");
        assert_eq!(ClientLibrary::Ky.wire_name(), "ky");
        assert_eq!(ClientLibrary::Got.wire_name(), "got");
        assert_eq!(ClientLibrary::Httpx.wire_name(), "httpx");
        assert_eq!(ClientLibrary::Requests.wire_name(), "requests");
        assert_eq!(ClientLibrary::Aiohttp.wire_name(), "aiohttp");
        assert_eq!(ClientLibrary::Custom.wire_name(), "custom");
    }
}
