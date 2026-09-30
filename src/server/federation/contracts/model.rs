//! Contract federation — per-repo facts.
//!
//! `ContractFact` and its payload types are the per-repo, sensor-written
//! facts that hang off a `GraphNode.contract` once a sensor has classified
//! it (HTTP route, HTTP client call, schema, field, field read, …). The
//! shape is fixed by `docs/CONTRACT_FEDERATION.md` §4.3 so every later
//! task — sensors, joiner, tools — speaks the same wire form.
//!
//! The companion types `ContractKey`, `JsonPath`, `ServiceName`, and the
//! `EndpointId` alias carry the derived keys used by the joiner
//! (federation/contracts/joiner.rs, task 7) and the on-the-wire command
//! tools (tasks 10+).
//!
//! Compatibility note. All enums use the default (externally tagged)
//! serde representation because bincode 2.x cannot decode internally
//! tagged or untagged variants. `#[serde(default)]` does not make
//! bincode files backward compatible; forward compatibility comes
//! only from the version bumps in §5.4. New variants go in
//! non-declaration order only when the wire shape requires it.

use serde::{Deserialize, Serialize};

use crate::federation::repo_id::RepoId;

// ─── Discriminated facts per node ──────────────────────────────────────

/// What a `GraphNode.contract` may say about its node.
///
/// Sensors write one of these variants on every node they own. The
/// payload is the sensor-specific fact (`ProviderFact`, `ConsumerFact`,
/// `Direction`, `FieldMeta`, `FieldReadFact`); the outer enum is what
/// `GraphNode.contract: Option<ContractFact>` actually carries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContractFact {
    /// HTTP provider endpoint — one declaration of an HTTP route.
    Provider(ProviderFact),
    /// HTTP consumer call site — one outbound `fetch` / `requests.get`
    /// / etc.
    Consumer(ConsumerFact),
    /// One request, response, or payload body of a route.
    Schema { direction: Direction },
    /// One flattened field of a `Schema`.
    Field(FieldMeta),
    /// One field read from a call's response.
    FieldRead(FieldReadFact),
}

// ─── HTTP provider ────────────────────────────────────────────────────

/// One provider endpoint declaration. Emitted by `http_sensor` and
/// `openapi_sensor`. `template` is the normalized route (§4.5).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderFact {
    pub method: HttpMethod,
    pub template: String, // normalized, same-file prefixes applied (§6.2)
    /// Code routes: the handler symbol. None for spec-only operations
    /// (OpenAPI documents an endpoint but no route in the repo claims
    /// it).
    pub handler: Option<SymbolKey>,
    /// OpenAPI operations: the `operationId`.
    pub operation_id: Option<String>,
    pub origin: ProviderOrigin, // Code | OpenApi
}

/// Source of a provider declaration. Distinct sensor owners (§6.1)
/// are derived from this: `Code` → `http_sensor`, `OpenApi` →
/// `openapi_sensor`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProviderOrigin {
    Code,
    OpenApi,
}

// ─── HTTP consumer ────────────────────────────────────────────────────

/// One outbound HTTP call site. Emitted by `http_client_sensor`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConsumerFact {
    /// `Known(HttpMethod)` for literal verbs; `Unknown` for
    /// non-literal expressions (e.g. `method=` arg).
    pub method: MethodSpec,
    pub url: NormalizedUrl,
    pub via: CallVia, // Library { name } | Receiver { expr, fn_name }
    /// Source text of the URL argument, truncated to 200 chars.
    pub url_expr: String,
    /// §6.5: `false` when the response escapes tracking (returned from
    /// a caller, passed out of scope, stored, spread, serialized,
    /// yielded, …). Reads and iteration are not escapes.
    pub reads_complete: bool,
}

/// How the call was made. `Library` is a known client
/// (`requests`, `axios`, `httpx`, …). `Receiver` is a wrapper
/// candidate that the joiner keeps only if `http_clients` matches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CallVia {
    Library { name: String },
    Receiver { expr: String, fn_name: String },
}

/// The HTTP verb, optionally unknown. `Known` is used for literal
/// verbs at a call site; `Unknown` is what `method=` (non-literal) or
/// arbitrary expression yields. Only `ContractKey::Http` for a
/// consumer key may carry `MethodSpec::Unknown` (§4.4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MethodSpec {
    Known(HttpMethod),
    Unknown,
}

/// Normalized URL carried on every `ConsumerFact`. `template = None`
/// means a fully dynamic path (no usable segments).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NormalizedUrl {
    pub host: HostPart, // None | Literal(String) | Env(Vec<String>) | Expr(String)
    pub template: Option<String>, // None = dynamic path
}

/// The host side of a `NormalizedUrl`. `Env` is the list of env-var
/// names the host expression resolves to via §6.3 host resolution.
/// `Expr` is the raw unresolved expression.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum HostPart {
    None,
    Literal(String),
    Env(Vec<String>),
    Expr(String),
}

// ─── Field reads ──────────────────────────────────────────────────────

/// One field read from a call's response. Emitted by
/// `field_access_sensor` per bound-identifier access. `exact` is true
/// when the read was on a bound identifier (§6.5).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FieldReadFact {
    pub chain: JsonPath,
    pub exact: bool,
}

// ─── Schema fields ────────────────────────────────────────────────────

/// One flattened field of a `Schema` (OpenAPI, …). Emitted by
/// `openapi_sensor`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FieldMeta {
    pub ty: TypeDesc,
    pub required: bool,
    pub nullable: bool,
    pub enum_values: Option<Vec<String>>,
}

/// JSON-Schema-style type for a field. `Array(Box<TypeDesc>)` carries
/// the element type. `Unknown` is the catch-all for OAS schemas
/// outside the recognized set (oneOf branches that disagree, foreign
/// `$ref`, …).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TypeDesc {
    String,
    Integer,
    Number,
    Boolean,
    Object,
    Array(Box<TypeDesc>),
    Unknown,
}

// ─── HTTP method, direction, entry kind ───────────────────────────────

/// One of the eight HTTP verbs. `Any` is the OpenAPI / Go-std
/// "no verb declared" case (§6.2).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
    Any,
}

/// Which side of an HTTP exchange a `Schema` belongs to. `Payload`
/// is the topic-event variant (§6.7, stretch).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Direction {
    Request,
    Response,
    Payload,
}

/// How a function is invoked at runtime. Set by `entry_point_sensor`
/// on `GraphNode.entry` (§6.6) so `used_by` (§10.9) can say why code
/// runs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EntryKind {
    HttpHandler,
    Scheduled,
    Cli,
    Main,
}

// ─── Source site ──────────────────────────────────────────────────────

/// Where in a file an edge originates. Carried on
/// `GraphEdge.site` for `SendsHttp`, `ReadsField`, and similar
/// per-call-site edges so `find_call_sites` can answer without a
/// separate map.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SourceSite {
    pub path: String,
    pub line: u32,
}

// ─── Identity helpers ─────────────────────────────────────────────────

/// Line-free identity of a function. `SymbolKey` is what survives
/// edits: confirmed bindings, `PathChanged` pairing, and consumer-
/// change pairing use it instead of `GlobalId`, whose last segment
/// is a line number and shifts on every move (§4.3 note).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SymbolKey {
    pub repo: RepoId,
    pub path: String,
    pub container: Option<String>,
    pub name: String,
}

// ─── Derived keys ─────────────────────────────────────────────────────

/// Federation-level identifier of an endpoint. `Http` keys are the
/// `(method, template)` pair; `Topic` keys are `(broker, name)`. The
/// `Display` / `FromStr` grammar is in §4.4 — implementations land
/// with the joiner (task 7) since they need the encoding machinery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ContractKey {
    /// HTTP endpoint. `MethodSpec::Unknown` only appears in
    /// consumer-side keys (§4.4).
    Http {
        method: MethodSpec,
        template: String,
    },
    Topic {
        broker: String,
        name: String,
    },
}

impl std::fmt::Display for ContractKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContractKey::Http { method, template } => {
                write!(f, "http:{} {}", method_label(method), template)
            }
            ContractKey::Topic { broker, name } => {
                write!(f, "topic:{}/{}", broker, name)
            }
        }
    }
}

impl std::str::FromStr for ContractKey {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(rest) = s.strip_prefix("http:") {
            // "<METHOD> <template>"
            let (method_str, template) = rest
                .split_once(' ')
                .ok_or_else(|| format!("malformed http key: {s:?}"))?;
            let method = parse_method_label(method_str)
                .ok_or_else(|| format!("unknown method in http key: {method_str:?}"))?;
            return Ok(ContractKey::Http {
                method,
                template: template.to_string(),
            });
        }
        if let Some(rest) = s.strip_prefix("topic:") {
            // "broker/name"
            let (broker, name) = rest
                .split_once('/')
                .ok_or_else(|| format!("malformed topic key: {s:?}"))?;
            return Ok(ContractKey::Topic {
                broker: broker.to_string(),
                name: name.to_string(),
            });
        }
        Err(format!("unknown contract key kind: {s:?}"))
    }
}

fn method_label(method: &MethodSpec) -> &'static str {
    match method {
        MethodSpec::Known(m) => match m {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Patch => "PATCH",
            HttpMethod::Delete => "DELETE",
            HttpMethod::Head => "HEAD",
            HttpMethod::Options => "OPTIONS",
            HttpMethod::Any => "ANY",
        },
        MethodSpec::Unknown => "UNKNOWN",
    }
}

fn parse_method_label(s: &str) -> Option<MethodSpec> {
    Some(match s {
        "GET" => MethodSpec::Known(HttpMethod::Get),
        "POST" => MethodSpec::Known(HttpMethod::Post),
        "PUT" => MethodSpec::Known(HttpMethod::Put),
        "PATCH" => MethodSpec::Known(HttpMethod::Patch),
        "DELETE" => MethodSpec::Known(HttpMethod::Delete),
        "HEAD" => MethodSpec::Known(HttpMethod::Head),
        "OPTIONS" => MethodSpec::Known(HttpMethod::Options),
        "ANY" => MethodSpec::Known(HttpMethod::Any),
        "UNKNOWN" => MethodSpec::Unknown,
        _ => return None,
    })
}

/// A JSON pointer (or, here, JSON path) into a payload. Segments are
/// name, array-element suffix, or map-values suffix. The `Display` /
/// `FromStr` grammar is in §4.4 — implementations land with the
/// joiner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JsonPath(pub Vec<PathSegment>);

/// One segment of a `JsonPath`. `Name` is a literal field name;
/// `ArrayItems` is the `[]` suffix; `MapValues` is the `{}` suffix.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PathSegment {
    Name(String),
    ArrayItems,
    MapValues,
}

impl std::fmt::Display for JsonPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for seg in &self.0 {
            match seg {
                PathSegment::Name(s) => {
                    if !first {
                        f.write_str(".")?;
                    }
                    write!(f, "{}", escape_name(s))?;
                    first = false;
                }
                // `[]` has no separator — `items[]` reads as "items
                // followed by array suffix". `{}` keeps the dot to
                // mirror the way an object property reads.
                PathSegment::ArrayItems => {
                    f.write_str("[]")?;
                    first = false;
                }
                PathSegment::MapValues => {
                    if !first {
                        f.write_str(".")?;
                    }
                    f.write_str("{}")?;
                    first = false;
                }
            }
        }
        Ok(())
    }
}

impl std::str::FromStr for JsonPath {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Ok(JsonPath(Vec::new()));
        }
        let mut segments = Vec::new();
        let mut chars = s.chars().peekable();
        let mut current = String::new();
        while let Some(&c) = chars.peek() {
            match c {
                '\\' => {
                    chars.next();
                    let esc = chars.next().ok_or_else(|| "trailing escape".to_string())?;
                    current.push(esc);
                }
                '.' => {
                    chars.next();
                    flush_name(&mut current, &mut segments)?;
                }
                '[' => {
                    chars.next();
                    if chars.peek() == Some(&']') {
                        chars.next();
                        flush_name(&mut current, &mut segments)?;
                        segments.push(PathSegment::ArrayItems);
                    } else {
                        current.push('[');
                    }
                }
                ']' => {
                    return Err("unescaped ']'".into());
                }
                '{' => {
                    chars.next();
                    if chars.peek() == Some(&'}') {
                        chars.next();
                        flush_name(&mut current, &mut segments)?;
                        segments.push(PathSegment::MapValues);
                    } else {
                        current.push('{');
                    }
                }
                '}' => return Err("unescaped '}'".into()),
                _ => {
                    chars.next();
                    current.push(c);
                }
            }
        }
        flush_name(&mut current, &mut segments)?;
        Ok(JsonPath(segments))
    }
}

fn escape_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 1);
    let mut chars = s.chars();
    if let Some(first) = chars.next() {
        // The reserved query sentinel (`$query`) is a literal
        // segment per §6.4, not a body property, so it must not be
        // escaped. Every other name starting with `$` carries the
        // `\$` escape per §6.4 "a body property whose name starts
        // with `$`".
        if first == '$' && s != "$query" {
            out.push('\\');
        }
        out.push(first);
    }
    for c in chars {
        if matches!(c, '.' | '[' | ']' | '{' | '}' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn flush_name(current: &mut String, segments: &mut Vec<PathSegment>) -> Result<(), String> {
    if current.is_empty() {
        return Ok(());
    }
    let name = std::mem::take(current);
    segments.push(PathSegment::Name(name));
    Ok(())
}

/// Federation-level service name. Validated as in §7.1
/// (`[a-z0-9][a-z0-9_-]*`). Validation lives with the config loader
/// (task 8); this newtype is just the wire shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ServiceName(pub String);

impl std::fmt::Display for ServiceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validation predicate used by the config loader. The match rule is
/// `^[a-z0-9][a-z0-9_-]*$` per §7.1. Public so the config loader can
/// call it without round-tripping through a `Config` type.
pub fn service_name_is_valid(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().expect("non-empty");
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
            return false;
        }
    }
    true
}

/// `(service, key)` uniquely identifies an endpoint in a federation.
/// The joiner indexes endpoints by this; tools serialize it as two
/// fields in every payload (§4.4).
pub type EndpointId = (ServiceName, ContractKey);
