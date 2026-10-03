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
    /// Topic subscription / publication site (§6.7, stretch). Emitted
    /// by `event_sensor` on the consuming function (the function
    /// whose body contains a `consumer.run` / `SubscribeTopics` /
    /// `@Cron` / Celery-`@app.task` site). The joiner (§7.7) matches
    /// these against producer-side `Provider` facts whose underlying
    /// `Topic` node carries the same `(broker, name)`.
    TopicConsumer(TopicConsumerFact),
    /// Phase D (spec §7): one database table surfaced by
    /// `sql_sensor`. `service` is filled in by the joiner when it
    /// links the table to its owning service via `repos.yaml` (the
    /// same mapping Phase B uses for HTTP clients); at scan time
    /// `service` is the empty string and the sensor records the
    /// table by name only. The sensor emits one `Table` node per
    /// distinct `(name, path)` pair so duplicate joins collapse.
    Table(Table),
    /// Phase E (spec §8.1, §8.2): one RPC method declared by a
    /// proto file (`service Foo { rpc Bar(...) returns (...); }`).
    /// The payload carries the same `(system, service, method)`
    /// triple the `ContractKey::Rpc` key is built from, plus the
    /// request/response type names the sensor extracted. `handler`
    /// is filled in by `grpc_provider_sensor`'s server-registration
    /// linkage (Task 3) when a `RegisterFooServer(...)` /
    /// `@GrpcService(impl = FooImpl.class)` /
    /// `add_FooServicer_to_server(...)` /
    /// `pb.RegisterFooServer(s, &fooImpl{})` call site resolves to
    /// a function node in the same repo.
    RpcProvider(RpcProviderFact),
    /// Phase E (spec §8.1, §8.2): one generated-stub call site
    /// (Go `client.Get(...)`, Java `ordersClient.getOrder(...)`,
    /// Python `stub.Get(...)`, C++ `stub->Get(...)`). The
    /// joiner (Task 5) resolves the receiver type + channel
    /// address to a `ContractKey::Rpc` provider via Phase B's
    /// wrapper resolution + Phase C's env aliases. Unresolvable
    /// stubs land in the coverage ledger as
    /// `Unresolved { reason: RpcStubUnknown }`.
    RpcConsumer(RpcConsumerFact),
    /// Phase E (spec §8.2): one server-registration link from a
    /// generated proto service to the user-supplied handler
    /// function. The payload carries the proto service key
    /// (`ContractKey::Rpc { system, service, method }`) and the
    /// handler function's `SymbolKey`. The sensor (Task 3) emits
    /// one per recognised registration pattern; the joiner
    /// records the link on the corresponding `RpcProvider` so
    /// typed traversal `handler → function → rpc` is reachable.
    RpcHandler(RpcHandlerFact),
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

// ─── RPC provider / consumer / handler (Phase E, spec §8) ─────────────

/// Phase E (spec §8.2): one RPC method declared in a proto file.
/// `service` is the proto package plus the service name joined by
/// `.` — `com.acme.orders.Orders` for
/// `package com.acme.orders; service Orders { … }`. `request_type`
/// and `response_type` are the proto type names the sensor
/// extracted; they are informational (the joiner matches on
/// `(service, method)` only). `handler` is filled by the
/// server-registration sensor (Task 3) once a registration call
/// resolves to a function node; `None` at scan time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RpcProviderFact {
    pub system: RpcSystem,
    pub service: String,
    pub method: String,
    pub request_type: String,
    pub response_type: String,
    pub handler: Option<SymbolKey>,
}

/// Phase E (spec §8.2): one generated-stub call site. `receiver` is
/// the typed stub (`ordersClient` / `stub` / `ordersStub`); `method`
/// is the bare rpc name. `channel_target` is the host:port literal
/// the sensor extracted from the channel construction
/// (`grpc.NewClient("orders:50051")`,
/// `ManagedChannelBuilder.forAddress("orders", 50051)`,
/// `grpc.insecure_channel("orders:50051")`). `channel_host_part`
/// is the same expression projected into the joiner's
/// `HostPart` shape so the existing tier-2 / tier-3 resolution
/// can run unchanged. A `None` channel means the call cannot be
/// resolved and the joiner must emit
/// `Unresolved { reason: RpcStubUnknown }`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RpcConsumerFact {
    pub system: RpcSystem,
    pub service: String,
    pub method: String,
    pub channel_target: Option<String>,
    pub channel_host_part: HostPart,
}

/// Phase E (spec §8.2): server-registration link. The sensor
/// emits one per detected registration call site. The joiner
/// records the link on the `RpcProvider` so typed traversal
/// `handler → function → rpc` is reachable. The `origin` is the
/// detected language-specific pattern (gRPC C++ `RegisterFooServer`
/// / gRPC Java Spring `@GrpcService` / gRPC Go `pb.RegisterFooServer`
/// / gRPC Python `add_FooServicer_to_server`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RpcHandlerFact {
    pub rpc_service: ContractKey,
    pub handler_function: SymbolKey,
    pub origin: RpcHandlerOrigin,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RpcHandlerOrigin {
    CppRegister,
    JavaGrpcService,
    GoRegister,
    PythonServicer,
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
///
/// Phase B (spec §5.2) extends `Receiver` with `base: Option<BaseOrigin>`
/// so the joiner can carry the originating `ClientDef` as evidence
/// on the resolved `Binds` edge. The field is `None` when the
/// sensor could not identify a `ClientDef` (the no-cross-file
/// resolution case, or the Python/TS case where `httpx.Client(...)`
/// exists but is not in the registry).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CallVia {
    Library {
        name: String,
    },
    Receiver {
        expr: String,
        fn_name: String,
        /// Phase B (spec §5.2): the originating `ClientDef` for the
        /// wrapper, when the joiner resolved one. `Some(origin)` →
        /// the joiner's evidence is a `Heuristic { detector: base_origin, ... }`
        /// bind; `None` → the call stays a candidate or unresolves
        /// the same way it would have pre-Phase-B.
        #[serde(default)]
        base: Option<BaseOrigin>,
    },
}

/// Phase B (spec §5.2) — the wrapper client a `CallVia::Receiver`
/// call originated from. Carried on the resolve so the user-facing
/// `EdgeProvenance::Heuristic { detector }` names the `ClientDef`
/// the joiner used. The `(client, module, site)` triple is enough
/// for the operator to jump back to the source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseOrigin {
    pub client: String,
    pub module: String,
    pub site: ClientSite,
}

/// Source site a `ClientDef` was detected at (spec §5.1). The
/// joiner carries this on `CallVia::Receiver::base` so the
/// user-visible `EdgeProvenance::Heuristic { detector }` names the
/// originating definition (spec §5.2 "evidence"). Lives in `model.rs`
/// (not `clients.rs`) so `CallVia::Receiver.base` can reference it
/// without a circular module dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientSite {
    pub path: String,
    pub line: u32,
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

/// Topic-side subscription or scheduler (§6.7, stretch). Mirrors the
/// `(broker, name)` of the producer-side `Topic` node. `kind` says
/// whether this consumer is a regular event subscriber
/// (`Subscription`) or a scheduled task (`Scheduled`, e.g. NestJS
/// `@Cron` / Celery `@app.task`). The joiner (`§7.7`) matches by
/// `(broker, name)` regardless of `kind`, so the variant is purely
/// descriptive — it lets tools render "subscription" vs "schedule"
/// without re-deriving the discriminator from the broker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopicConsumerFact {
    pub broker: String,
    pub name: String,
    pub kind: TopicConsumerKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TopicConsumerKind {
    Subscription,
    Scheduled,
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

// ─── SQL tables (Phase D, spec §7) ────────────────────────────────────

/// Phase D (spec §7): one database table surfaced by `sql_sensor`.
///
/// `name` is the literal SQL identifier the sensor extracted from
/// a SQL statement — `orders` from `SELECT id FROM orders`. It is
/// stored verbatim (case preserved) so the joiner can apply the
/// repo's case-folding rules if any.
///
/// `service` names the repo / service that owns the table. At
/// scan time the sensor has no service context — `sql_sensor`
/// sees the literal SQL and the source file, not `repos.yaml`.
/// The joiner fills `service` from `repos.yaml#services[]` the
/// same way Phase B links an HTTP-client wrapper to its owning
/// service. Before the joiner runs, `service` is the empty
/// string, and downstream tools render the table as `orders`
/// (by name only). The joiner's `rejoin_contracts` step is what
/// promotes the empty string into the service name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Table {
    pub service: String,
    pub name: String,
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
/// `(method, template)` pair; `Topic` keys are `(broker, name)`;
/// `Rpc` keys are `(system, package.service, method)` per spec §8.1.
/// The `Display` / `FromStr` grammar is in §4.4 — implementations
/// land with the joiner (task 7) since they need the encoding
/// machinery.
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
    /// Phase E (spec §8.1, §8.2): one RPC method on a (possibly
    /// package-qualified) service. `service` is the proto package
    /// plus service name joined by `.` — `com.acme.orders.Orders`
    /// for `package com.acme.orders; service Orders { … }`. The
    /// method is the rpc's bare name (no input/output type). The
    /// wire-form `display` projects `service.method` so the
    /// joiner and the contract-federation tools can render it
    /// without the `system` prefix.
    Rpc {
        system: RpcSystem,
        service: String,
        method: String,
    },
}

/// Phase E (spec §8.1): the RPC family. Only `Grpc` exists today
/// (spec §8.3 is GraphQL which uses a separate `ContractKey`
/// variant). The enum is open so future systems (Thrift,
/// Connect-RPC) extend without a wire-shape change beyond the
/// discriminator.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RpcSystem {
    Grpc,
}

impl std::fmt::Display for RpcSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcSystem::Grpc => f.write_str("grpc"),
        }
    }
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
            ContractKey::Rpc { service, method, .. } => {
                write!(f, "rpc:{}/{}", service, method)
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
        if let Some(rest) = s.strip_prefix("rpc:") {
            // "<service>/<method>" (service is package-qualified).
            let (service, method) = rest
                .split_once('/')
                .ok_or_else(|| format!("malformed rpc key: {s:?}"))?;
            return Ok(ContractKey::Rpc {
                system: RpcSystem::Grpc,
                service: service.to_string(),
                method: method.to_string(),
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
