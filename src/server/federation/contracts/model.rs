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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Direction {
    Request,
    Response,
    Payload,
}

/// How a function is invoked at runtime. Set by `entry_point_sensor`
/// on `GraphNode.entry` (§6.6) so `used_by` (§10.9) can say why code
/// runs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
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

/// A JSON pointer (or, here, JSON path) into a payload. Segments are
/// name, array-element suffix, or map-values suffix. The `Display` /
/// `FromStr` grammar is in §4.4 — implementations land with the
/// joiner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct JsonPath(pub Vec<PathSegment>);

/// One segment of a `JsonPath`. `Name` is a literal field name;
/// `ArrayItems` is the `[]` suffix; `MapValues` is the `{}` suffix.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum PathSegment {
    Name(String),
    ArrayItems,
    MapValues,
}

/// Federation-level service name. Validated as in §7.1
/// (`[a-z0-9][a-z0-9_-]*`). Validation lives with the config loader
/// (task 8); this newtype is just the wire shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ServiceName(pub String);

/// `(service, key)` uniquely identifies an endpoint in a federation.
/// The joiner indexes endpoints by this; tools serialize it as two
/// fields in every payload (§4.4).
pub type EndpointId = (ServiceName, ContractKey);
