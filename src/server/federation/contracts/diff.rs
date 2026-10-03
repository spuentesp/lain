//! Contract diff and evaluation (`docs/CONTRACT_FEDERATION.md` §9).
//!
//! Pure functions only. Inputs are `ContractIndex`-derived surfaces
//! plus an injected changed-files source for `ChangedWithoutSchema`
//! (§9.2). No graph I/O, no mirror reads, no sensor walks.
//!
//! The module is split into:
//!
//! - `surface`: extract `ContractSurface` from a `ContractIndex` (§9.1).
//! - `change`: classify endpoint and consumer changes into `ChangeKind`s (§9.2, §9.3).
//! - `classify`: the §9.4 `Compat` table.
//! - `evaluate`: the §9.5 trace, per-consumer table, and could-match rule.
//! - `coverage`: §9.6 / §9.7.
//!
//! `diff_contracts(a, a)` is empty by construction; `evaluate` is a
//! pure function of `(change, base_view, head_view, changed_files)`.
//!
//! PR 13 will wire the real git2/mirror lookup behind
//! `ChangedFilesSource::from_git` (marked `// TODO(PR 13)`).

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::federation::contracts::index::{
    ConsumerResolution, ConsumerTarget as IndexConsumerTarget, ContractIndex, Endpoint, EndpointId,
    EndpointSchema, UnresolvedReason,
};
use crate::federation::contracts::model::{
    ContractKey, Direction, FieldMeta, JsonPath, MethodSpec, ServiceName, SymbolKey, TypeDesc,
};
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::schema::EdgeProvenance;

// ─── Surfaces (§9.1) ──────────────────────────────────────────────────

/// Per-revision federation surface. Endpoints are keyed per service
/// (§9.1: the same `ContractKey` served by two services never
/// collides). Consumers are keyed by `ConsumerKey` — the caller
/// `SymbolKey` plus the `ContractKey` (or `url_expr` when the call has
/// no `ContractKey`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContractSurface {
    pub endpoints: BTreeMap<EndpointId, EndpointDef>,
    pub consumers: BTreeMap<ConsumerKey, ConsumerDef>,
}

/// One endpoint in a surface. Schemas are keyed by `Direction` then
/// JSON path. `has_schema` is true when at least one direction has
/// fields. `source_files` is the union of every route/spec/handler
/// file the joiner observed for this endpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndpointDef {
    pub providers: Vec<ProviderRef>,
    pub schemas: BTreeMap<Direction, BTreeMap<JsonPath, FieldMeta>>,
    pub has_schema: bool,
    pub source_files: BTreeSet<String>,
}

/// Provider record on a surface. `GlobalId` of the route / spec node;
/// `handler` is the code `SymbolKey`; `operation_id` is the OpenAPI
/// id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRef {
    pub node_id: GlobalId,
    pub handler: Option<SymbolKey>,
    pub operation_id: Option<String>,
}

/// Surface key for a consumer. `(SymbolKey, ContractKey or url_expr)`
/// per §9.1 — `ContractKey` is the resolved key when present,
/// otherwise the raw `url_expr` text used to key unresolvable calls.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConsumerKey {
    pub caller: SymbolKey,
    pub target: ConsumerTargetKey,
}

/// The second half of a `ConsumerKey`. `Contract` carries the resolved
/// `ContractKey`; `UrlExpr` carries the raw `url_expr` text for calls
/// that resolve to no `ContractKey` (unresolvable, generic-key, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ConsumerTargetKey {
    Contract(ContractKey),
    UrlExpr(String),
}

/// One consumer in a surface. `call` is the `HttpClientCall` node id;
/// `resolution` records the joiner's verdict on it (`Binds`,
/// `External`, `Unresolved`); `reads` is the set of `JsonPath`s the
/// reading function accessed via `FieldRef` chains;
/// `reads_complete` mirrors `ConsumerFact.reads_complete` (§6.5) —
/// `false` means the response escaped tracking and the read set is
/// only partial.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsumerDef {
    pub call: GlobalId,
    pub resolution: SurfaceResolution,
    pub reads: BTreeSet<JsonPath>,
    pub reads_complete: bool,
}

/// Surface-side resolution mirroring the joiner's `ConsumerTarget`.
/// `External` and `Unresolved` consumers are still indexed by key so
/// the consumer-side diff can detect when one appears only in head
/// (§9.3).
#[derive(Debug, Clone, PartialEq)]
pub enum SurfaceResolution {
    Binds {
        /// Endpoint ids the consumer resolves to (one or several on
        /// ambiguity, exactly one on a confirmed binding).
        endpoints: Vec<EndpointId>,
        /// The edge provenance, used by §9.5 `certain`.
        provenance: EdgeProvenance,
    },
    External {
        host: String,
    },
    Unresolved {
        reason: UnresolvedReason,
        /// `Some(s)` when the joiner knew the target service but
        /// found no matching route (§7.3 rule 3). `None` when the
        /// target service is unknown (§7.3 rule 6 / rule 5).
        target_service: Option<ServiceName>,
    },
}

// ─── ContractSurface extraction (§9.1) ────────────────────────────────

impl ContractSurface {
    /// Extract a `ContractSurface` from a view's `ContractIndex`.
    /// Pure function of the index. Endpoints are kept keyed per
    /// service; consumers are projected from `ConsumerResolution`
    /// plus the joiner's `FieldRef`s (one read per `FieldRef` whose
    /// `ReadsFrom` targets this consumer).
    pub fn from_index(index: &ContractIndex) -> Self {
        let mut surface = ContractSurface::default();
        for (endpoint_id, endpoint) in &index.endpoints {
            surface
                .endpoints
                .insert(endpoint_id.clone(), endpoint_to_def(endpoint));
        }
        // Project every consumer. Field reads (`field_refs`) are
        // recorded for the §9.5 `reads` set: each `FieldRefResolution`
        // carries the `ReadsFrom` call id, and the read chain is
        // the FieldRef's node name (§4.4 JsonPath grammar).
        // Unbound reads count too — §9.3's `ConsumerFieldUnmatched`
        // fires on a head read the schema never covered (scenario
        // 20's `discount`).
        let mut reads_by_call: BTreeMap<GlobalId, BTreeSet<JsonPath>> = BTreeMap::new();
        for (fid, fr) in &index.field_refs {
            if fr.call.is_empty() {
                continue;
            }
            let Ok(call_id) = GlobalId::parse(&fr.call) else {
                continue;
            };
            let Some(name) = fid.name() else {
                continue;
            };
            let Ok(path) = JsonPath::from_str(&name) else {
                continue;
            };
            reads_by_call.entry(call_id).or_default().insert(path);
        }
        for (call_id, resolution) in &index.consumers {
            let def = consumer_to_def(call_id, resolution, &reads_by_call);
            let key = consumer_key(call_id, resolution);
            surface.consumers.insert(key, def);
        }
        surface
    }
}

fn endpoint_to_def(endpoint: &Endpoint) -> EndpointDef {
    let providers: Vec<ProviderRef> = endpoint
        .providers
        .iter()
        .map(|p| ProviderRef {
            node_id: p.node_id.clone(),
            handler: p.handler.clone(),
            operation_id: p.operation_id.clone(),
        })
        .collect();
    let mut schemas: BTreeMap<Direction, BTreeMap<JsonPath, FieldMeta>> = BTreeMap::new();
    for (dir, schema) in &endpoint.schemas {
        let fields = schema_to_fields(schema);
        schemas.insert(*dir, fields);
    }
    let has_schema = !schemas.is_empty() && schemas.values().any(|f| !f.is_empty());
    // §9.2 `ChangedWithoutSchema`: the file(s) holding the bound
    // handler SymbolKey when set (one file per code-bound provider),
    // else the route/spec node's path for spec-only providers. Plus
    // the schema node's path (where field metadata lives). Tied to
    // the BOUND handler so an unrelated edit to a shared module file
    // or routing table does not fire the rule for endpoints whose
    // contract did not actually change.
    let mut source_files: BTreeSet<String> = BTreeSet::new();
    for p in &endpoint.providers {
        let file = p
            .handler
            .as_ref()
            .map(|h| h.path.clone())
            .or_else(|| p.node_id.path());
        if let Some(file) = file {
            source_files.insert(file);
        }
    }
    for schema in endpoint.schemas.values() {
        if let Some(path) = schema.node_id.path() {
            source_files.insert(path);
        }
    }
    EndpointDef {
        providers,
        schemas,
        has_schema,
        source_files,
    }
}

fn schema_to_fields(schema: &EndpointSchema) -> BTreeMap<JsonPath, FieldMeta> {
    schema.fields.clone()
}

fn consumer_to_def(
    call_id: &GlobalId,
    resolution: &ConsumerResolution,
    reads_by_call: &BTreeMap<GlobalId, BTreeSet<JsonPath>>,
) -> ConsumerDef {
    let surface_resolution = match &resolution.target {
        Some(IndexConsumerTarget::Binds { provenance, .. }) => SurfaceResolution::Binds {
            endpoints: resolution.bound_endpoints.clone(),
            provenance: provenance.clone(),
        },
        Some(IndexConsumerTarget::External { host }) => {
            SurfaceResolution::External { host: host.clone() }
        }
        Some(IndexConsumerTarget::Unresolved {
            reason,
            target_service,
        }) => SurfaceResolution::Unresolved {
            reason: *reason,
            target_service: target_service.clone(),
        },
        None => SurfaceResolution::Unresolved {
            reason: UnresolvedReason::NoMatch,
            target_service: None,
        },
    };
    ConsumerDef {
        call: call_id.clone(),
        resolution: surface_resolution,
        reads: reads_by_call.get(call_id).cloned().unwrap_or_default(),
        reads_complete: resolution.reads_complete,
    }
}

/// Build a `ConsumerKey` for a `ConsumerResolution`. The caller
/// `SymbolKey` is reconstructed from the call's `GlobalId`
/// (path/name/line) — the joiner's `ConfirmedBinding` mechanism uses
/// the same lookup. Resolved calls use the resolved `ContractKey`;
/// unresolved ones fall back to the `url_expr` text.
fn consumer_key(call_id: &GlobalId, resolution: &ConsumerResolution) -> ConsumerKey {
    let repo = RepoId::new(call_id.repo_id()).unwrap_or_else(|_| RepoId::new("unknown").unwrap());
    let caller = SymbolKey {
        repo,
        path: call_id.path().unwrap_or_default(),
        container: None,
        name: call_id.name().unwrap_or_default(),
    };
    let target = match &resolution.target {
        Some(IndexConsumerTarget::Binds { .. }) => match resolution.bound_endpoints.first() {
            Some((_, key)) => ConsumerTargetKey::Contract(key.clone()),
            None => ConsumerTargetKey::UrlExpr(call_id.name().unwrap_or_default()),
        },
        Some(IndexConsumerTarget::External { host }) => ConsumerTargetKey::UrlExpr(host.clone()),
        Some(IndexConsumerTarget::Unresolved { .. }) => {
            ConsumerTargetKey::UrlExpr(call_id.name().unwrap_or_default())
        }
        None => ConsumerTargetKey::UrlExpr(call_id.name().unwrap_or_default()),
    };
    ConsumerKey { caller, target }
}

// ─── ChangeKind (§9.2, §9.3) ──────────────────────────────────────────

/// One diff entry. Provider-side kinds apply to endpoints; consumer-
/// side kinds apply to consumers. The accompanying `service` and
/// `endpoint_id` (or `consumer` key) make the entry locatable in the
/// `Impact` it eventually produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub service: ServiceName,
    pub kind: ChangeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    /// §9.2: an endpoint removed and one added in the same service
    /// share a handler `SymbolKey` or `operation_id`.
    PathChanged { from: ContractKey, to: ContractKey },
    /// §9.2: same handler but only the method differs.
    MethodChanged { from: ContractKey, to: ContractKey },
    /// §9.2: an endpoint removed with no pairing.
    EndpointRemoved { key: ContractKey },
    /// §9.2: an endpoint added with no pairing.
    EndpointAdded { key: ContractKey },
    /// §9.2: a per-endpoint + per-direction field removed.
    FieldRemoved {
        endpoint: EndpointId,
        direction: Direction,
        path: JsonPath,
    },
    /// §9.2: a per-endpoint + per-direction field added. `required`
    /// reflects the head's `FieldMeta.required`.
    FieldAdded {
        endpoint: EndpointId,
        direction: Direction,
        path: JsonPath,
        required: bool,
    },
    /// §9.2: under one parent path + direction, exactly one removed
    /// and one added with equal `TypeDesc`. `required` mirrors the
    /// destination `FieldMeta.required` so §9.4's request-side
    /// classify cell can branch on the new field's requiredness.
    FieldRenamed {
        endpoint: EndpointId,
        direction: Direction,
        from: JsonPath,
        to: JsonPath,
        required: bool,
    },
    /// §9.2: same path, `TypeDesc` differs. `required` mirrors the
    /// destination `FieldMeta.required` so §9.4's request-side
    /// classify cell can branch on the new field's requiredness.
    FieldTypeChanged {
        endpoint: EndpointId,
        direction: Direction,
        path: JsonPath,
        from: TypeDesc,
        to: TypeDesc,
        required: bool,
    },
    /// §9.2: same path, requiredness differs.
    RequirednessChanged {
        endpoint: EndpointId,
        direction: Direction,
        path: JsonPath,
        now_required: bool,
    },
    /// §9.2: same path, nullability differs.
    NullabilityChanged {
        endpoint: EndpointId,
        direction: Direction,
        path: JsonPath,
        now_nullable: bool,
    },
    /// §9.2: enum value removed.
    EnumValueRemoved {
        endpoint: EndpointId,
        direction: Direction,
        path: JsonPath,
        value: String,
    },
    /// §9.2: enum value added.
    EnumValueAdded {
        endpoint: EndpointId,
        direction: Direction,
        path: JsonPath,
        value: String,
    },
    /// §9.2: handler change on an endpoint with no schema. Emitted
    /// only when `ChangedFilesSource` reports a file in
    /// `source_files` differs between base and head.
    ChangedWithoutSchema { endpoint: EndpointId },
    /// §9.3: consumer in head but not in base, unresolved in head.
    ConsumerEndpointUnmatched { consumer: ConsumerKey },
    /// §9.3: head consumer bound to an endpoint with a schema reads
    /// a path the schema does not contain, and base did not.
    ConsumerFieldUnmatched {
        consumer: ConsumerKey,
        field: JsonPath,
    },
    /// §9.3: same key bound to a different endpoint; informational,
    /// `Compatible`.
    ConsumerRebound {
        consumer: ConsumerKey,
        from: EndpointId,
        to: EndpointId,
    },
}

// ─── changed-files source injection ──────────────────────────────────

/// Injected source for the `ChangedWithoutSchema` rule (§9.2). The
/// `git2`-backed mirror implementation lives in
/// [`crate::federation::contracts::changed_files`]; the trait is the
/// pure-function seam tests use.
pub trait ChangedFilesSource {
    fn changed_files(&self, base: &str, head: &str) -> BTreeSet<String>;
}

/// A precomputed changed-files set. Used by tests and by callers that
/// already hold a `BTreeSet<String>` from another source.
pub struct StaticChangedFiles(pub BTreeSet<String>);

impl ChangedFilesSource for StaticChangedFiles {
    fn changed_files(&self, _base: &str, _head: &str) -> BTreeSet<String> {
        self.0.clone()
    }
}

/// `BTreeSet`-based source injected into `diff_contracts`. Same shape
/// as the `StaticChangedFiles` indirection; kept distinct so PR 13
/// can replace the trait without breaking test call sites.
pub fn changed_files_set(set: BTreeSet<String>) -> impl ChangedFilesSource {
    StaticChangedFiles(set)
}

// ─── diff_contracts (§9.2) ────────────────────────────────────────────

/// Diff provider-side endpoints between `base` and `head` (§9.2).
/// Pure function of `(base, head, changed_files)`; `diff_contracts(a,
/// a)` is empty by construction.
///
/// Pairing happens in two stages:
/// 1. Match removed+added endpoints in the same service by shared
///    `SymbolKey` (handler) or shared `operation_id`.
/// 2. Anything left unmatched is reported as `EndpointRemoved` /
///    `EndpointAdded`.
///
/// Field-level diffs apply per `(endpoint, direction)` to the **head**
/// endpoints. `PathChanged` / `MethodChanged` carry their own
/// `(from, to)` pair so the field diff runs on the **head**
/// endpoint's id.
pub fn diff_contracts(
    base: &ContractSurface,
    head: &ContractSurface,
    changed_files: &dyn ChangedFilesSource,
) -> Vec<Change> {
    let mut changes: Vec<Change> = Vec::new();
    let mut head_remaining: BTreeMap<EndpointId, EndpointDef> = head.endpoints.clone();
    let mut base_remaining: BTreeMap<EndpointId, EndpointDef> = base.endpoints.clone();

    // Stage 1 — pair removed+added by exact (service, key) match first,
    // then by shared handler / operation_id. Exact matches that share
    // the same key produce no `PathChanged` / `MethodChanged` change
    // (they're the same endpoint).
    let mut pairings: Vec<(EndpointId, EndpointId)> = Vec::new();
    let base_ids: Vec<EndpointId> = base.endpoints.keys().cloned().collect();
    for base_id in &base_ids {
        if !base_remaining.contains_key(base_id) {
            continue;
        }
        let base_def = &base.endpoints[base_id];
        // Exact match — same key on both sides is a no-op (same
        // endpoint). Pair it and skip change emission.
        if let Some(head_id) = head_remaining.keys().find(|h| *h == base_id).cloned() {
            pairings.push((base_id.clone(), head_id.clone()));
            base_remaining.remove(base_id);
            head_remaining.remove(&head_id);
            continue;
        }
        let mut paired: Option<EndpointId> = None;
        for head_id in head.endpoints.keys() {
            if head_id.0 != base_id.0 {
                continue;
            }
            if !head_remaining.contains_key(head_id) {
                continue;
            }
            let head_def = &head.endpoints[head_id];
            if shares_handler_or_operation_id(base_def, head_def) {
                paired = Some(head_id.clone());
                break;
            }
        }
        if let Some(head_id) = paired {
            pairings.push((base_id.clone(), head_id.clone()));
            base_remaining.remove(base_id);
            head_remaining.remove(&head_id);
        }
    }

    // Stage 2 — unpaired removals and additions.
    for base_id in base_remaining.keys() {
        changes.push(Change {
            service: base_id.0.clone(),
            kind: ChangeKind::EndpointRemoved {
                key: base_id.1.clone(),
            },
        });
    }
    for head_id in head_remaining.keys() {
        changes.push(Change {
            service: head_id.0.clone(),
            kind: ChangeKind::EndpointAdded {
                key: head_id.1.clone(),
            },
        });
    }

    // Stage 3 — field-level diffs for paired endpoints.
    for (base_id, head_id) in &pairings {
        let base_def = &base.endpoints[base_id];
        let head_def = &head.endpoints[head_id];
        // Determine if this is a PathChanged or MethodChanged pair.
        let base_method = method_of(base_id);
        let head_method = method_of(head_id);
        let path_changed = base_id.1 != head_id.1;
        let method_changed = base_method != head_method;
        if path_changed && !method_changed {
            changes.push(Change {
                service: head_id.0.clone(),
                kind: ChangeKind::PathChanged {
                    from: base_id.1.clone(),
                    to: head_id.1.clone(),
                },
            });
        } else if method_changed && !path_changed {
            changes.push(Change {
                service: head_id.0.clone(),
                kind: ChangeKind::MethodChanged {
                    from: base_id.1.clone(),
                    to: head_id.1.clone(),
                },
            });
        }
        // Field-level diff (per direction).
        for direction in [Direction::Request, Direction::Response, Direction::Payload] {
            diff_fields(
                &mut changes,
                head_id,
                direction,
                base_def.schemas.get(&direction),
                head_def.schemas.get(&direction),
            );
        }
        // ChangedWithoutSchema rule — both sides have no schema and a
        // source file differs.
        if !base_def.has_schema && !head_def.has_schema {
            let base_sha = "";
            let head_sha = "";
            let changed = changed_files.changed_files(base_sha, head_sha);
            // The probe is "did any file in `source_files` (route,
            // spec, or handler) differ between base and head?".
            // Either side's `source_files` entry matching is
            // sufficient — `ChangedFilesSource` is the caller-supplied
            // git2-backed source that returns the changed set for
            // `(base_sha, head_sha)`.
            if base_def
                .source_files
                .iter()
                .chain(head_def.source_files.iter())
                .any(|f| changed.contains(f))
            {
                changes.push(Change {
                    service: head_id.0.clone(),
                    kind: ChangeKind::ChangedWithoutSchema {
                        endpoint: head_id.clone(),
                    },
                });
            }
        }
    }

    changes.sort_by(|a, b| {
        change_sort_key(&a.service, &a.kind).cmp(&change_sort_key(&b.service, &b.kind))
    });
    changes
}

fn change_sort_key(service: &ServiceName, kind: &ChangeKind) -> String {
    service.to_string() + "|" + &kind_label(kind)
}

fn kind_label(kind: &ChangeKind) -> String {
    match kind {
        ChangeKind::PathChanged { from, to } => format!("path:{from}->{to}"),
        ChangeKind::MethodChanged { from, to } => format!("method:{from}->{to}"),
        ChangeKind::EndpointRemoved { key } => format!("removed:{key}"),
        ChangeKind::EndpointAdded { key } => format!("added:{key}"),
        ChangeKind::FieldRemoved {
            endpoint,
            direction,
            path,
        } => {
            format!(
                "field-removed:{}:{}:{path}",
                endpoint.1,
                direction_label(*direction)
            )
        }
        ChangeKind::FieldAdded {
            endpoint,
            direction,
            path,
            ..
        } => {
            format!(
                "field-added:{}:{}:{path}",
                endpoint.1,
                direction_label(*direction)
            )
        }
        ChangeKind::FieldRenamed {
            endpoint,
            direction,
            from,
            to,
            ..
        } => {
            format!(
                "field-renamed:{}:{}:{from}->{to}",
                endpoint.1,
                direction_label(*direction)
            )
        }
        ChangeKind::FieldTypeChanged {
            endpoint,
            direction,
            path,
            ..
        } => {
            format!(
                "field-type:{}:{}:{path}",
                endpoint.1,
                direction_label(*direction)
            )
        }
        ChangeKind::RequirednessChanged {
            endpoint,
            direction,
            path,
            ..
        } => {
            format!(
                "requiredness:{}:{}:{path}",
                endpoint.1,
                direction_label(*direction)
            )
        }
        ChangeKind::NullabilityChanged {
            endpoint,
            direction,
            path,
            ..
        } => {
            format!(
                "nullability:{}:{}:{path}",
                endpoint.1,
                direction_label(*direction)
            )
        }
        ChangeKind::EnumValueRemoved {
            endpoint,
            direction,
            path,
            value,
        } => {
            format!(
                "enum-removed:{}:{}:{path}:{value}",
                endpoint.1,
                direction_label(*direction)
            )
        }
        ChangeKind::EnumValueAdded {
            endpoint,
            direction,
            path,
            value,
        } => {
            format!(
                "enum-added:{}:{}:{path}:{value}",
                endpoint.1,
                direction_label(*direction)
            )
        }
        ChangeKind::ChangedWithoutSchema { endpoint } => {
            format!("changed-no-schema:{}", endpoint.1)
        }
        ChangeKind::ConsumerEndpointUnmatched { consumer } => {
            format!("consumer-unmatched:{}", consumer_label(consumer))
        }
        ChangeKind::ConsumerFieldUnmatched { consumer, field } => {
            format!(
                "consumer-field-unmatched:{}:{field}",
                consumer_label(consumer)
            )
        }
        ChangeKind::ConsumerRebound { consumer, from, to } => {
            format!(
                "consumer-rebound:{}:{}->{}",
                consumer_label(consumer),
                from.1,
                to.1
            )
        }
    }
}

fn consumer_label(c: &ConsumerKey) -> String {
    format!("{}:{}", c.caller.path, c.caller.name)
}

fn direction_label(d: Direction) -> &'static str {
    match d {
        Direction::Request => "request",
        Direction::Response => "response",
        Direction::Payload => "payload",
    }
}

fn method_of(id: &EndpointId) -> Option<crate::federation::contracts::model::HttpMethod> {
    match &id.1 {
        ContractKey::Http { method, .. } => match method {
            MethodSpec::Known(m) => Some(*m),
            MethodSpec::Unknown => None,
        },
        ContractKey::Topic { .. } | ContractKey::Rpc { .. } => None,
    }
}

fn shares_handler_or_operation_id(a: &EndpointDef, b: &EndpointDef) -> bool {
    for pa in &a.providers {
        for pb in &b.providers {
            if let (Some(ha), Some(hb)) = (&pa.handler, &pb.handler) {
                if ha == hb {
                    return true;
                }
            }
            if let (Some(oa), Some(ob)) = (&pa.operation_id, &pb.operation_id) {
                if oa == ob && !oa.is_empty() {
                    return true;
                }
            }
        }
    }
    false
}

pub(crate) fn diff_fields(
    out: &mut Vec<Change>,
    endpoint: &EndpointId,
    direction: Direction,
    base: Option<&BTreeMap<JsonPath, FieldMeta>>,
    head: Option<&BTreeMap<JsonPath, FieldMeta>>,
) {
    let base = base.cloned().unwrap_or_default();
    let head = head.cloned().unwrap_or_default();

    // Detect renames first (single remove + single add with equal
    // `TypeDesc` under the same parent). The nested-field rule
    // collapses descendant removals onto the topmost removed path.
    let removed_paths = removed_paths(&base, &head);
    let added_paths = added_paths(&base, &head);

    // Field-level removed.
    for path in &removed_paths {
        if let Some(meta) = base.get(path) {
            out.push(Change {
                service: endpoint.0.clone(),
                kind: ChangeKind::FieldRemoved {
                    endpoint: endpoint.clone(),
                    direction,
                    path: path.clone(),
                },
            });
            // Detect attribute changes between base[path] and head[path]
            // if the path was kept (e.g. type changed on the same key).
            let _ = meta;
        }
    }
    // Field-level added.
    for path in &added_paths {
        let required = head.get(path).map(|m| m.required).unwrap_or(false);
        out.push(Change {
            service: endpoint.0.clone(),
            kind: ChangeKind::FieldAdded {
                endpoint: endpoint.clone(),
                direction,
                path: path.clone(),
                required,
            },
        });
    }

    // Renames: a path appears as removed and a different path appears
    // as added under the same parent with equal `TypeDesc`.
    let mut rename_from: BTreeSet<JsonPath> = BTreeSet::new();
    let mut rename_to: BTreeSet<JsonPath> = BTreeSet::new();
    for r in &removed_paths {
        for a in &added_paths {
            if !same_parent(r, a) {
                continue;
            }
            let base_meta = match base.get(r) {
                Some(m) => m,
                None => continue,
            };
            let head_meta = match head.get(a) {
                Some(m) => m,
                None => continue,
            };
            if base_meta.ty == head_meta.ty && !rename_from.contains(r) && !rename_to.contains(a) {
                rename_from.insert(r.clone());
                rename_to.insert(a.clone());
                break;
            }
        }
    }
    // Replace the FieldRemoved/FieldAdded entries that compose a
    // rename with a single FieldRenamed entry.
    let mut i = 0;
    while i < out.len() {
        let drop = matches!(
            &out[i].kind,
            ChangeKind::FieldRemoved { endpoint: e, direction: d, path }
                if *e == *endpoint && *d == direction && rename_from.contains(path)
        );
        if drop {
            out.remove(i);
        } else {
            i += 1;
        }
    }
    i = 0;
    while i < out.len() {
        let drop = matches!(
            &out[i].kind,
            ChangeKind::FieldAdded { endpoint: e, direction: d, path, .. }
                if *e == *endpoint && *d == direction && rename_to.contains(path)
        );
        if drop {
            out.remove(i);
        } else {
            i += 1;
        }
    }
    for (from, to) in rename_from.iter().zip(rename_to.iter()) {
        let required = head.get(to).map(|m| m.required).unwrap_or(false);
        out.push(Change {
            service: endpoint.0.clone(),
            kind: ChangeKind::FieldRenamed {
                endpoint: endpoint.clone(),
                direction,
                from: from.clone(),
                to: to.clone(),
                required,
            },
        });
    }

    // Attribute changes for paths present on both sides. Nested-field
    // rule: a path present on both sides is reported as a change
    // (we don't suppress it); an object-field removal collapses onto
    // the topmost path (`removed_paths` already does this).
    let mut common: Vec<&JsonPath> = base.keys().filter(|k| head.contains_key(*k)).collect();
    common.sort();
    for path in common {
        let base_meta = &base[path];
        let head_meta = &head[path];
        if base_meta.ty != head_meta.ty {
            out.push(Change {
                service: endpoint.0.clone(),
                kind: ChangeKind::FieldTypeChanged {
                    endpoint: endpoint.clone(),
                    direction,
                    path: path.clone(),
                    from: base_meta.ty.clone(),
                    to: head_meta.ty.clone(),
                    required: head_meta.required,
                },
            });
        }
        if base_meta.required != head_meta.required {
            out.push(Change {
                service: endpoint.0.clone(),
                kind: ChangeKind::RequirednessChanged {
                    endpoint: endpoint.clone(),
                    direction,
                    path: path.clone(),
                    now_required: head_meta.required,
                },
            });
        }
        if base_meta.nullable != head_meta.nullable {
            out.push(Change {
                service: endpoint.0.clone(),
                kind: ChangeKind::NullabilityChanged {
                    endpoint: endpoint.clone(),
                    direction,
                    path: path.clone(),
                    now_nullable: head_meta.nullable,
                },
            });
        }
        if let (Some(bev), Some(hev)) = (&base_meta.enum_values, &head_meta.enum_values) {
            let base_set: BTreeSet<&String> = bev.iter().collect();
            let head_set: BTreeSet<&String> = hev.iter().collect();
            for v in base_set.difference(&head_set) {
                out.push(Change {
                    service: endpoint.0.clone(),
                    kind: ChangeKind::EnumValueRemoved {
                        endpoint: endpoint.clone(),
                        direction,
                        path: path.clone(),
                        value: (*v).clone(),
                    },
                });
            }
            for v in head_set.difference(&base_set) {
                out.push(Change {
                    service: endpoint.0.clone(),
                    kind: ChangeKind::EnumValueAdded {
                        endpoint: endpoint.clone(),
                        direction,
                        path: path.clone(),
                        value: (*v).clone(),
                    },
                });
            }
        } else if base_meta.enum_values.is_some() != head_meta.enum_values.is_some() {
            // One side has enum, the other doesn't — emit a single
            // TypeChanged to capture the structural drop.
            out.push(Change {
                service: endpoint.0.clone(),
                kind: ChangeKind::FieldTypeChanged {
                    endpoint: endpoint.clone(),
                    direction,
                    path: path.clone(),
                    from: base_meta.ty.clone(),
                    to: head_meta.ty.clone(),
                    required: head_meta.required,
                },
            });
        }
    }
}

fn removed_paths(
    base: &BTreeMap<JsonPath, FieldMeta>,
    head: &BTreeMap<JsonPath, FieldMeta>,
) -> BTreeSet<JsonPath> {
    let mut out: BTreeSet<JsonPath> = BTreeSet::new();
    for path in base.keys() {
        if head.contains_key(path) {
            continue;
        }
        // Nested-field rule: if any ancestor of `path` was also
        // removed, this path is reported by the topmost ancestor and
        // is skipped here.
        if has_removed_ancestor(path, base, head) {
            continue;
        }
        out.insert(path.clone());
    }
    out
}

fn added_paths(
    base: &BTreeMap<JsonPath, FieldMeta>,
    head: &BTreeMap<JsonPath, FieldMeta>,
) -> BTreeSet<JsonPath> {
    let mut out: BTreeSet<JsonPath> = BTreeSet::new();
    for path in head.keys() {
        if base.contains_key(path) {
            continue;
        }
        if has_added_ancestor(path, base, head) {
            continue;
        }
        out.insert(path.clone());
    }
    out
}

fn has_removed_ancestor(
    path: &JsonPath,
    base: &BTreeMap<JsonPath, FieldMeta>,
    head: &BTreeMap<JsonPath, FieldMeta>,
) -> bool {
    let mut prefix = JsonPath(Vec::new());
    for (i, _) in path.0.iter().enumerate() {
        if i == 0 {
            continue;
        }
        prefix.0 = path.0[..i].to_vec();
        if base.contains_key(&prefix) && !head.contains_key(&prefix) {
            return true;
        }
    }
    false
}

fn has_added_ancestor(
    path: &JsonPath,
    base: &BTreeMap<JsonPath, FieldMeta>,
    head: &BTreeMap<JsonPath, FieldMeta>,
) -> bool {
    let mut prefix = JsonPath(Vec::new());
    for (i, _) in path.0.iter().enumerate() {
        if i == 0 {
            continue;
        }
        prefix.0 = path.0[..i].to_vec();
        if head.contains_key(&prefix) && !base.contains_key(&prefix) {
            return true;
        }
    }
    false
}

fn same_parent(a: &JsonPath, b: &JsonPath) -> bool {
    let pa = parent_of(a);
    let pb = parent_of(b);
    pa == pb
}

fn parent_of(p: &JsonPath) -> JsonPath {
    if p.0.len() <= 1 {
        JsonPath(Vec::new())
    } else {
        JsonPath(p.0[..p.0.len() - 1].to_vec())
    }
}

// ─── Consumer-side diff (§9.3) ───────────────────────────────────────

/// Diff consumers between base and head (§9.3). Detects:
/// - `ConsumerEndpointUnmatched`: a head-only key with an unresolved
///   resolution (`NoRouteInService` / `NoMatch`).
/// - `ConsumerFieldUnmatched`: a head consumer whose bound endpoint's
///   schema does not contain a path the consumer reads and the base
///   did not.
/// - `ConsumerRebound`: same key bound to a different endpoint id.
pub fn diff_consumers(base: &ContractSurface, head: &ContractSurface) -> Vec<Change> {
    let mut out: Vec<Change> = Vec::new();
    for (key, head_def) in &head.consumers {
        let base_def = base.consumers.get(key);
        match &head_def.resolution {
            SurfaceResolution::Unresolved { reason, .. } => {
                if matches!(
                    reason,
                    UnresolvedReason::NoRouteInService | UnresolvedReason::NoMatch
                ) && base_def.is_none()
                {
                    out.push(Change {
                        service: consumer_service(key),
                        kind: ChangeKind::ConsumerEndpointUnmatched {
                            consumer: key.clone(),
                        },
                    });
                }
            }
            SurfaceResolution::Binds { ref endpoints, .. } => {
                if let Some(base_def) = base_def {
                    if let SurfaceResolution::Binds {
                        endpoints: base_endpoints,
                        ..
                    } = &base_def.resolution
                    {
                        if base_endpoints != endpoints {
                            if let (Some(from), Some(to)) =
                                (base_endpoints.first(), endpoints.first())
                            {
                                if from != to {
                                    out.push(Change {
                                        service: consumer_service(key),
                                        kind: ChangeKind::ConsumerRebound {
                                            consumer: key.clone(),
                                            from: from.clone(),
                                            to: to.clone(),
                                        },
                                    });
                                }
                            }
                        }
                    }
                }
                // ConsumerFieldUnmatched — bound endpoint has a
                // schema, head reads a path that schema does not
                // contain, and base did not read it. The base-read
                // check matters now that the surface carries real
                // reads: a read that was already unmatched in base
                // (e.g. the sensor's `r.json()` chain) is not a
                // *change*, while scenario 20's new `discount` read
                // is.
                let base_reads: BTreeSet<crate::federation::contracts::model::JsonPath> =
                    base_def.map(|b| b.reads.clone()).unwrap_or_default();
                for endpoint_id in endpoints {
                    if let Some(endpoint) = head.endpoints.get(endpoint_id) {
                        if !endpoint.has_schema {
                            continue;
                        }
                        for read in &head_def.reads {
                            if base_reads.contains(read) {
                                continue;
                            }
                            let in_head_schema = endpoint
                                .schemas
                                .values()
                                .any(|fields| fields.contains_key(read));
                            let in_base_schema = base
                                .endpoints
                                .get(endpoint_id)
                                .map(|e| e.schemas.values().any(|fields| fields.contains_key(read)))
                                .unwrap_or(false);
                            if !in_head_schema && !in_base_schema {
                                out.push(Change {
                                    service: consumer_service(key),
                                    kind: ChangeKind::ConsumerFieldUnmatched {
                                        consumer: key.clone(),
                                        field: read.clone(),
                                    },
                                });
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out.sort_by(|a, b| {
        change_sort_key(&a.service, &a.kind).cmp(&change_sort_key(&b.service, &b.kind))
    });
    out
}

fn consumer_service(key: &ConsumerKey) -> ServiceName {
    ServiceName(key.caller.repo.as_str().to_string())
}

// ─── classify (§9.4) ──────────────────────────────────────────────────

/// The five values of the `Compat` enum (§9.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Compat {
    Compatible,
    Breaking,
    BreakingIfRead,
    BreakingIfSent,
    NeedsReview,
}

impl Compat {
    pub fn as_str(&self) -> &'static str {
        match self {
            Compat::Compatible => "Compatible",
            Compat::Breaking => "Breaking",
            Compat::BreakingIfRead => "BreakingIfRead",
            Compat::BreakingIfSent => "BreakingIfSent",
            Compat::NeedsReview => "NeedsReview",
        }
    }
}

/// Map a `ChangeKind` + `Direction` to its `Compat` (§9.4 table, every
/// cell verbatim). The §9.4 table's "Response / payload" column
/// groups `Direction::Response` and `Direction::Payload` together;
/// `Direction::Request` is its own column.
pub fn classify(kind: &ChangeKind, direction: Direction) -> Compat {
    use ChangeKind::*;
    match kind {
        FieldRemoved { .. } => match direction {
            Direction::Request => Compat::Compatible,
            Direction::Response | Direction::Payload => Compat::BreakingIfRead,
        },
        FieldAdded { required, .. } => match direction {
            Direction::Request => {
                if *required {
                    Compat::Breaking
                } else {
                    Compat::Compatible
                }
            }
            Direction::Response | Direction::Payload => Compat::Compatible,
        },
        FieldRenamed { required, .. } => match direction {
            Direction::Request => {
                // §9.4: "Breaking if the new field is required, else
                // NeedsReview". `required` is the destination
                // `FieldMeta.required`.
                if *required {
                    Compat::Breaking
                } else {
                    Compat::NeedsReview
                }
            }
            Direction::Response | Direction::Payload => Compat::BreakingIfRead,
        },
        FieldTypeChanged { required, .. } => match direction {
            Direction::Request => {
                // §9.4: "Breaking if required, else BreakingIfSent".
                if *required {
                    Compat::Breaking
                } else {
                    Compat::BreakingIfSent
                }
            }
            Direction::Response | Direction::Payload => Compat::BreakingIfRead,
        },
        RequirednessChanged { now_required, .. } => match direction {
            Direction::Response | Direction::Payload => {
                if *now_required {
                    // §9.4: "became required" → response/payload
                    // Compatible.
                    Compat::Compatible
                } else {
                    // became optional → response/payload
                    // BreakingIfRead.
                    Compat::BreakingIfRead
                }
            }
            Direction::Request => {
                if *now_required {
                    Compat::Breaking
                } else {
                    Compat::Compatible
                }
            }
        },
        NullabilityChanged { now_nullable, .. } => match direction {
            Direction::Response | Direction::Payload => {
                if *now_nullable {
                    // §9.4: "became nullable" → response/payload
                    // BreakingIfRead.
                    Compat::BreakingIfRead
                } else {
                    // §9.4: "became non-nullable" → response/payload
                    // Compatible.
                    Compat::Compatible
                }
            }
            Direction::Request => {
                if *now_nullable {
                    Compat::Compatible
                } else {
                    Compat::BreakingIfSent
                }
            }
        },
        EnumValueRemoved { .. } => match direction {
            Direction::Response | Direction::Payload => Compat::Compatible,
            Direction::Request => Compat::BreakingIfSent,
        },
        EnumValueAdded { .. } => match direction {
            Direction::Response | Direction::Payload => Compat::NeedsReview,
            Direction::Request => Compat::Compatible,
        },
        EndpointRemoved { .. } | PathChanged { .. } | MethodChanged { .. } => Compat::Breaking,
        EndpointAdded { .. } => Compat::Compatible,
        ChangedWithoutSchema { .. } => Compat::NeedsReview,
        ConsumerEndpointUnmatched { .. } | ConsumerFieldUnmatched { .. } => Compat::Breaking,
        ConsumerRebound { .. } => Compat::Compatible,
    }
}

// ─── Impact (§9.5) ──────────────────────────────────────────────────

/// The verdict of `evaluate(change)`. `Class` is the strongest class
/// over consumers (`Verified > NeedsInvestigation > NoKnownImpact`).
/// `affected` lists every consumer that contributed to the verdict
/// (skipping `Compatible`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Impact {
    pub service: ServiceName,
    pub kind: ChangeKind,
    pub class: Class,
    pub reason: Option<Reason>,
    pub affected: Vec<Affected>,
    pub scope: Scope,
    pub coverage: Coverage,
    pub compatible_changes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    NoKnownImpact,
    NeedsInvestigation,
    Verified,
}

impl Class {
    pub fn as_str(&self) -> &'static str {
        match self {
            Class::Verified => "Verified",
            Class::NeedsInvestigation => "NeedsInvestigation",
            Class::NoKnownImpact => "NoKnownImpact",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    HeuristicBinding,
    SendsNotModeled,
    ReadsNotFullyTraced,
    NeedsReview,
    NoSchema,
    UnresolvedCandidates,
}

impl Reason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Reason::HeuristicBinding => "heuristic_binding",
            Reason::SendsNotModeled => "sends_not_modeled",
            Reason::ReadsNotFullyTraced => "reads_not_fully_traced",
            Reason::NeedsReview => "needs_review",
            Reason::NoSchema => "no_schema",
            Reason::UnresolvedCandidates => "unresolved_candidates",
        }
    }
}

/// One bound consumer affected by the change, with its own class and
/// the reason that pushed it there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Affected {
    pub service: ServiceName,
    pub consumer: ConsumerKey,
    pub class: Class,
    pub reason: Reason,
}

/// §9.6 scope: every `NoKnownImpact` carries one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    pub reviewed: Vec<ReviewedRepo>,
    pub unreviewed: Vec<UnreviewedRepo>,
    pub configured_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewedRepo {
    pub repo: String,
    pub commit: Option<String>,
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreviewedRepo {
    pub repo: String,
    pub reason: String,
    pub error: Option<String>,
}

/// §9.7 coverage: every analysis returns one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coverage {
    pub repos: Vec<RepoCoverage>,
    pub unresolved_consumers: Vec<ConsumerKey>,
    pub ambiguous: Vec<ConsumerKey>,
    pub unnormalized: Vec<ConsumerKey>,
    pub external: Vec<(String, u32)>,
    pub stale_bindings: u32,
    pub schemaless_endpoints: Vec<EndpointId>,
    pub scope: Scope,
    pub complete: bool,
    /// TLA+ CoverageClaim.tla: the per-repo `RepoCoverage` state
    /// vectors. Phase A wires the coverage ledger into the
    /// evaluator's `is_complete` check (Task 7) — empty here means
    /// "no ledger attached", which the evaluator treats as
    /// pre-Phase-A behaviour (no coverage downgrade). When the
    /// federation hydrates from the per-commit cache, every in-scope
    /// repo carries an entry.
    pub repo_coverages:
        std::collections::BTreeMap<String, crate::federation::contracts::coverage::RepoCoverage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoCoverage {
    pub repo: String,
    pub commit: Option<String>,
    pub state: String,
    pub sensor_counts: BTreeMap<String, u32>,
    pub error: Option<String>,
}

// ─── evaluate (§9.5) ──────────────────────────────────────────────────

/// Compute the §9.5 impact for a single `Change`. Pure function of
/// `(change, base, head, coverage)` — the caller assembles the
/// `Coverage` (§9.7) from the federation's repo states and from the
/// `ContractIndex`.
pub fn evaluate(
    change: &Change,
    base: &ContractSurface,
    head: &ContractSurface,
    coverage: &Coverage,
) -> Impact {
    let mut affected: Vec<Affected> = Vec::new();
    let mut compatible_changes: u32 = 0;
    let compat = change_compat(change);

    // Consumer-side changes short-circuit with their own verdict
    // (§9.5: "the consumer's target service is resolved Static or
    // Confirmed and that service's repo is reviewed").
    if let kind @ (ChangeKind::ConsumerEndpointUnmatched { .. }
    | ChangeKind::ConsumerFieldUnmatched { .. }
    | ChangeKind::ConsumerRebound { .. }) = &change.kind
    {
        let consumer = match kind {
            ChangeKind::ConsumerEndpointUnmatched { consumer }
            | ChangeKind::ConsumerFieldUnmatched { consumer, .. }
            | ChangeKind::ConsumerRebound { consumer, .. } => consumer.clone(),
            _ => unreachable!(),
        };
        let head_def = head.consumers.get(&consumer);
        let class = consumer_side_class(head_def, coverage);
        let reason = match class {
            Class::Verified => Reason::HeuristicBinding,
            Class::NeedsInvestigation => Reason::HeuristicBinding,
            Class::NoKnownImpact => Reason::HeuristicBinding,
        };
        affected.push(Affected {
            service: consumer_service(&consumer),
            consumer,
            class,
            reason,
        });
        let class = strongest(&affected);
        let reason = affected.first().map(|a| a.reason);
        return finalize_impact(
            change,
            class,
            reason,
            affected,
            coverage,
            compatible_changes,
        );
    }

    // Provider-side change — §9.5 says we trace the OLD contract in
    // BASE "where the old contract and its consumers exist". The
    // per-consumer table therefore iterates the consumers bound in
    // `base`, not `head`. The head surface is consulted only when a
    // consumer no longer binds in head (its URL or template moved) —
    // for those the reads set comes from base. The `reads_complete`
    // mirror lives on the per-base `ConsumerDef`; a consumer dropped
    // from head without a head-side equivalent keeps base's
    // `reads_complete` (escapes from base still escape in head).
    let target_endpoint = provider_target_endpoint(change);

    // Provider-side per-consumer trace base (§9.5). The "target"
    // is the changed endpoint. For PathChanged / MethodChanged /
    // EndpointRemoved the binding the consumer held in base can be
    // the pre-rename id, so the filter accepts consumers bound to
    // either the `to` key (head) or, when the change kind has a
    // distinct `from`, the `from` key. For EndpointRemoved the
    // `from` key is the removed endpoint id itself.
    let secondary_target = match &change.kind {
        ChangeKind::PathChanged { .. } | ChangeKind::MethodChanged { .. } => {
            Some(change_secondary_target(change))
        }
        _ => None,
    };
    let removed_target = match &change.kind {
        ChangeKind::EndpointRemoved { key } => Some((change.service.clone(), key.clone())),
        _ => None,
    };

    let base_target_keys: Vec<&EndpointId> = match &change.kind {
        ChangeKind::PathChanged { .. } | ChangeKind::MethodChanged { .. } => {
            let mut v: Vec<&EndpointId> = Vec::new();
            if let Some(t) = target_endpoint.as_ref() {
                v.push(t);
            }
            if let Some(s) = secondary_target.as_ref() {
                v.push(s);
            }
            v.dedup();
            v
        }
        _ => {
            if let Some(removed) = removed_target.as_ref() {
                vec![removed]
            } else {
                target_endpoint
                    .as_ref()
                    .map(|t| vec![t])
                    .unwrap_or_default()
            }
        }
    };

    let base_consumers_for_endpoint: Vec<(&ConsumerKey, &ConsumerDef)> =
        if !base_target_keys.is_empty() {
            base.consumers
                .iter()
                .filter(|(_, c)| match &c.resolution {
                    SurfaceResolution::Binds { endpoints, .. } => {
                        base_target_keys.iter().any(|t| endpoints.contains(t))
                    }
                    _ => false,
                })
                .collect()
        } else {
            Vec::new()
        };

    let mut class_overall = Class::NoKnownImpact;
    let mut reason_overall: Option<Reason> = None;
    let mut any_affected = false;
    for (ckey, cdef) in &base_consumers_for_endpoint {
        // Look up the same consumer in head; if absent, fall back to
        // the base-side `ConsumerDef`. `reads_complete` stays
        // identical between base and head — the sensor's escape
        // rules (§6.5) are deterministic on the response's binding.
        let head_def = head.consumers.get(*ckey);
        let cdef_for_verdict = head_def.unwrap_or(cdef);
        let base_reads = cdef.reads.clone();
        let reads_complete = cdef_for_verdict.reads_complete();
        let reads_field = reads_field_for(change, cdef_for_verdict, cdef, base_reads);
        let (class, reason) = per_consumer_verdict(
            compat,
            change,
            reads_field,
            reads_complete,
            cdef_for_verdict,
        );
        if matches!(class, Class::NoKnownImpact) {
            continue;
        }
        any_affected = true;
        if class >= class_overall {
            class_overall = class;
            reason_overall = Some(reason);
        }
        affected.push(Affected {
            service: consumer_service(ckey),
            consumer: (*ckey).clone(),
            class,
            reason,
        });
    }

    // Compatible → counted only. `Compatible` is the only class that
    // increments `compatible_changes`; Breaking/BreakingIfRead/
    // BreakingIfSent/NeedsReview with no affected consumer fall
    // through to the could-match rule.
    if !any_affected {
        // No bound consumer is affected. The could-match rule
        // applies first (§9.5): an unresolved consumer in a reviewed
        // repo that **could match** the change makes the verdict
        // `NeedsInvestigation (unresolved_candidates)` regardless of
        // the change's `Compat` classification.
        if let Some(target) = &target_endpoint {
            let candidates: Vec<ConsumerKey> = coverage
                .unresolved_consumers
                .iter()
                .filter(|u| could_match(u, target, head))
                .cloned()
                .collect();
            if !candidates.is_empty() {
                class_overall = Class::NeedsInvestigation;
                reason_overall = Some(Reason::UnresolvedCandidates);
                for c in candidates {
                    affected.push(Affected {
                        service: consumer_service(&c),
                        consumer: c,
                        class: Class::NeedsInvestigation,
                        reason: Reason::UnresolvedCandidates,
                    });
                }
            } else if matches!(compat, Compat::Compatible) {
                compatible_changes = 1;
                class_overall = Class::NoKnownImpact;
                reason_overall = None;
            } else if matches!(compat, Compat::BreakingIfSent) {
                // §15.2 row 19: a `BreakingIfSent` change is a lead
                // even when no bound consumer exists to enumerate —
                //0.9 cannot model what callers send.
                class_overall = Class::NeedsInvestigation;
                reason_overall = Some(Reason::SendsNotModeled);
            } else {
                class_overall = Class::NoKnownImpact;
                reason_overall = None;
            }
        } else if matches!(compat, Compat::Compatible) {
            compatible_changes = 1;
            class_overall = Class::NoKnownImpact;
            reason_overall = None;
        } else if matches!(compat, Compat::BreakingIfSent) {
            class_overall = Class::NeedsInvestigation;
            reason_overall = Some(Reason::SendsNotModeled);
        } else {
            class_overall = Class::NoKnownImpact;
            reason_overall = None;
        }
    }

    // TLA+ CoverageClaim.tla `NoKnownImpactSound`: when `claim_fired`
    // (here, `NoKnownImpact`), every in-scope repo must be `RepoComplete(r)`.
    // Phase A enforces this in `evaluate()`: if the coverage ledger is
    // attached AND any in-scope repo's `RepoCoverage::is_complete` is false
    // (missing ledger entry, sensor error, unresolved could-match,
    // cache-version mismatch), downgrade `NoKnownImpact` →
    // `NeedsInvestigation` with `Reason::UnresolvedCandidates`. The
    // downgrade is opt-in via `coverage.repo_coverages` being non-empty
    // so existing tests that do not wire a ledger preserve the
    // pre-Phase-A verdicts.
    if matches!(class_overall, Class::NoKnownImpact) && !coverage.repo_coverages.is_empty() {
        let current_analyzer_version = crate::federation::contracts::analyzer_version();
        let capable = crate::federation::contracts::coverage::consumer_capable_langs();
        let mut incomplete_repos: Vec<String> = Vec::new();
        for repo in scope_repo_names(coverage) {
            let Some(cover) = coverage.repo_coverages.get(&repo) else {
                incomplete_repos.push(repo);
                continue;
            };
            if !cover.is_complete(&capable, &current_analyzer_version) {
                incomplete_repos.push(repo);
            }
        }
        if !incomplete_repos.is_empty() {
            class_overall = Class::NeedsInvestigation;
            reason_overall = Some(Reason::UnresolvedCandidates);
            affected.push(Affected {
                service: change.service.clone(),
                consumer: ConsumerKey {
                    caller: crate::federation::contracts::model::SymbolKey {
                        repo: crate::federation::repo_id::RepoId::new("coverage").unwrap_or_else(
                            |_| crate::federation::repo_id::RepoId::new("unknown").unwrap(),
                        ),
                        path: "<coverage>".into(),
                        container: None,
                        name: "<incomplete_repo>".into(),
                    },
                    target: ConsumerTargetKey::UrlExpr(format!(
                        "incomplete:{}",
                        incomplete_repos.join(",")
                    )),
                },
                class: Class::NeedsInvestigation,
                reason: Reason::UnresolvedCandidates,
            });
        }
    }

    finalize_impact(
        change,
        class_overall,
        reason_overall,
        affected,
        coverage,
        compatible_changes,
    )
}

/// TLA+: every repo in scope is `(reviewed ∪ unreviewed)`. The
/// `scope_is_complete` check iterates this set against the ledger.
/// Phase A pins the names from the existing `Scope` struct so the
/// legacy builders that pass an empty ledger still pass
/// `repo_coverages.is_empty()` and skip the downgrade.
fn scope_repo_names(coverage: &Coverage) -> Vec<String> {
    let mut names: Vec<String> = coverage
        .scope
        .reviewed
        .iter()
        .map(|r| r.repo.clone())
        .collect();
    for r in &coverage.scope.unreviewed {
        names.push(r.repo.clone());
    }
    names.sort();
    names.dedup();
    names
}

fn finalize_impact(
    change: &Change,
    class: Class,
    reason: Option<Reason>,
    affected: Vec<Affected>,
    coverage: &Coverage,
    compatible_changes: u32,
) -> Impact {
    Impact {
        service: change.service.clone(),
        kind: change.kind.clone(),
        class,
        reason,
        affected,
        scope: coverage.scope.clone(),
        coverage: coverage.clone(),
        compatible_changes,
    }
}

impl ConsumerDef {
    /// §9.5's `reads_complete` for the per-consumer table — direct
    /// passthrough to the field of the same name.
    pub fn reads_complete(&self) -> bool {
        self.reads_complete
    }
}

/// For a `PathChanged` / `MethodChanged`, the `from` endpoint id is
/// the one the consumer was bound to in base. Used to widen the
/// base-consumers filter to the pre-rename key.
fn change_secondary_target(change: &Change) -> EndpointId {
    match &change.kind {
        ChangeKind::PathChanged { from, .. } | ChangeKind::MethodChanged { from, .. } => {
            (change.service.clone(), from.clone())
        }
        _ => panic!("change_secondary_target called for non-Path/Method change"),
    }
}

fn provider_target_endpoint(change: &Change) -> Option<EndpointId> {
    match &change.kind {
        ChangeKind::PathChanged { to, .. }
        | ChangeKind::MethodChanged { to, .. }
        | ChangeKind::EndpointAdded { key: to } => Some((change.service.clone(), to.clone())),
        ChangeKind::FieldRemoved { endpoint, .. }
        | ChangeKind::FieldAdded { endpoint, .. }
        | ChangeKind::FieldRenamed { endpoint, .. }
        | ChangeKind::FieldTypeChanged { endpoint, .. }
        | ChangeKind::RequirednessChanged { endpoint, .. }
        | ChangeKind::NullabilityChanged { endpoint, .. }
        | ChangeKind::EnumValueRemoved { endpoint, .. }
        | ChangeKind::EnumValueAdded { endpoint, .. }
        | ChangeKind::ChangedWithoutSchema { endpoint } => Some(endpoint.clone()),
        // EndpointRemoved carries the removed key on its `service`;
        // the endpoint was live in base, so we project it as the
        // target so the could-match rule (and the per-base-consumer
        // trace) can still fire on the removed endpoint id.
        ChangeKind::EndpointRemoved { key } => Some((change.service.clone(), key.clone())),
        _ => None,
    }
}

fn change_compat(change: &Change) -> Compat {
    match &change.kind {
        ChangeKind::FieldRemoved { direction, .. }
        | ChangeKind::FieldAdded { direction, .. }
        | ChangeKind::FieldRenamed { direction, .. }
        | ChangeKind::FieldTypeChanged { direction, .. }
        | ChangeKind::RequirednessChanged { direction, .. }
        | ChangeKind::NullabilityChanged { direction, .. }
        | ChangeKind::EnumValueRemoved { direction, .. }
        | ChangeKind::EnumValueAdded { direction, .. } => classify(&change.kind, *direction),
        _ => classify(&change.kind, Direction::Response),
    }
}

fn reads_field_for(
    change: &Change,
    head_consumer: &ConsumerDef,
    base_consumer: &ConsumerDef,
    _base_reads: BTreeSet<JsonPath>,
) -> bool {
    // For a rename, the "changed path" is both `from` (still read by
    // stale consumers in base) and `to` (the new path head-side
    // consumers read). §9.5: "A consumer 'reads' a changed field
    // when it reads that path or any path under it" — for a rename
    // we treat both names as the changed set, since the response
    // shape is one field with two names during the migration.
    match &change.kind {
        ChangeKind::FieldRenamed { from, to, .. } => {
            let head_reads = &head_consumer.reads;
            if head_reads
                .iter()
                .any(|r| path_is_ancestor(to, r) || r == to)
            {
                return true;
            }
            let base_reads = &base_consumer.reads;
            if base_reads
                .iter()
                .any(|r| path_is_ancestor(from, r) || r == from)
            {
                return true;
            }
            false
        }
        ChangeKind::FieldRemoved { path, .. }
        | ChangeKind::FieldAdded { path, .. }
        | ChangeKind::FieldTypeChanged { path, .. }
        | ChangeKind::RequirednessChanged { path, .. }
        | ChangeKind::NullabilityChanged { path, .. }
        | ChangeKind::EnumValueRemoved { path, .. }
        | ChangeKind::EnumValueAdded { path, .. } => {
            let head_reads = &head_consumer.reads;
            if head_reads
                .iter()
                .any(|r| path_is_ancestor(path, r) || r == path)
            {
                return true;
            }
            false
        }
        _ => false,
    }
}

fn path_is_ancestor(parent: &JsonPath, descendant: &JsonPath) -> bool {
    parent.0.len() < descendant.0.len() && descendant.0.starts_with(&parent.0)
}

fn per_consumer_verdict(
    compat: Compat,
    change: &Change,
    reads: bool,
    reads_complete: bool,
    consumer: &ConsumerDef,
) -> (Class, Reason) {
    let certain = is_certain(consumer);
    match compat {
        Compat::Breaking => {
            if certain {
                (Class::Verified, Reason::HeuristicBinding)
            } else {
                (Class::NeedsInvestigation, Reason::HeuristicBinding)
            }
        }
        Compat::BreakingIfRead => {
            if reads {
                // The consumer reads the field. `reads_complete`
                // governs whether we trust the read set: a stored /
                // serialized / yielded response (`reads_complete =
                // false`, §6.5) means the read set may be partial —
                // we don't know whether the consumer actually saw
                // this field. §9.5 row "reads": Verified if certain
                // (Static/Confirmed) else NI heuristic_binding, but
                // scenario 22 / §9.5's `reads_not_fully_traced`
                // extends the conservative verdict to the
                // `reads_complete = false` case here.
                if !reads_complete {
                    (Class::NeedsInvestigation, Reason::ReadsNotFullyTraced)
                } else if certain {
                    (Class::Verified, Reason::HeuristicBinding)
                } else {
                    (Class::NeedsInvestigation, Reason::HeuristicBinding)
                }
            } else if reads_complete {
                (Class::NoKnownImpact, Reason::HeuristicBinding)
            } else {
                (Class::NeedsInvestigation, Reason::ReadsNotFullyTraced)
            }
        }
        Compat::BreakingIfSent => (Class::NeedsInvestigation, Reason::SendsNotModeled),
        Compat::NeedsReview => match change.kind {
            ChangeKind::EnumValueAdded { .. } => {
                if reads {
                    (Class::NeedsInvestigation, Reason::NeedsReview)
                } else if reads_complete {
                    (Class::NoKnownImpact, Reason::NeedsReview)
                } else {
                    (Class::NeedsInvestigation, Reason::ReadsNotFullyTraced)
                }
            }
            ChangeKind::FieldRenamed { .. } => {
                // §9.5 row "NeedsReview request": NI needs_review for
                // every bound consumer (regardless of reads).
                let _ = (reads, reads_complete);
                (Class::NeedsInvestigation, Reason::NeedsReview)
            }
            ChangeKind::ChangedWithoutSchema { .. } => {
                (Class::NeedsInvestigation, Reason::NoSchema)
            }
            _ => (Class::NeedsInvestigation, Reason::NeedsReview),
        },
        Compat::Compatible => (Class::NoKnownImpact, Reason::HeuristicBinding),
    }
}

fn is_certain(consumer: &ConsumerDef) -> bool {
    match &consumer.resolution {
        SurfaceResolution::Binds { provenance, .. } => match provenance {
            EdgeProvenance::Static { .. } => true,
            EdgeProvenance::Confirmed { .. } => true,
            EdgeProvenance::Heuristic { .. } => false,
            EdgeProvenance::Runtime { .. } => false,
        },
        _ => false,
    }
}

fn consumer_side_class(consumer: Option<&ConsumerDef>, coverage: &Coverage) -> Class {
    let Some(consumer) = consumer else {
        return Class::NeedsInvestigation;
    };
    // The "target service" is the service the consumer is calling —
    // i.e. the endpoint's service — not the call's own repo.
    let target_service = match &consumer.resolution {
        SurfaceResolution::Binds { endpoints, .. } => match endpoints.first() {
            Some((svc, _)) => svc.0.clone(),
            None => return Class::NeedsInvestigation,
        },
        _ => return Class::NeedsInvestigation,
    };
    let reviewed = coverage
        .scope
        .reviewed
        .iter()
        .any(|r| r.repo == target_service);
    let resolved_static = matches!(
        consumer.resolution,
        SurfaceResolution::Binds {
            provenance: EdgeProvenance::Static { .. } | EdgeProvenance::Confirmed { .. },
            ..
        }
    );
    if reviewed && resolved_static {
        Class::Verified
    } else {
        Class::NeedsInvestigation
    }
}

fn strongest(affected: &[Affected]) -> Class {
    let mut c = Class::NoKnownImpact;
    for a in affected {
        if a.class > c {
            c = a.class;
        }
    }
    c
}

// ─── could-match rule (§9.7) ─────────────────────────────────────────

/// An unresolved HTTP consumer `u` in a reviewed repo **could match**
/// a change on endpoint `(s, K)` iff `u` is not external, `u`'s
/// target service is `s` or unknown, `u`'s method equals K's or one
/// of them is `Unknown` or `ANY`, and `u`'s template is `None` or
/// matches K by §7.4 (prefix tolerance included).
pub fn could_match(
    consumer_key: &ConsumerKey,
    endpoint: &EndpointId,
    head: &ContractSurface,
) -> bool {
    let Some(consumer) = head.consumers.get(consumer_key) else {
        return false;
    };
    // Not external.
    if matches!(consumer.resolution, SurfaceResolution::External { .. }) {
        return false;
    }
    // Service-name condition (§9.7): "u's target service is `s` or
    // unknown". Resolved consumers carry the bound endpoint's
    // service; unresolved consumers carry either the joiner-known
    // target service (`Some`) or unknown (`None`). A consumer whose
    // `target_service` is `Some(other)` does NOT could-match an
    // `s` endpoint — its known target rules out the change.
    let target_service_ok = match &consumer.resolution {
        SurfaceResolution::Binds { endpoints, .. } => match endpoints.first() {
            Some((svc, _)) => svc.0 == endpoint.0 .0,
            None => false,
        },
        SurfaceResolution::Unresolved { target_service, .. } => match target_service {
            Some(s) => s.0 == endpoint.0 .0,
            None => true,
        },
        SurfaceResolution::External { .. } => false,
    };
    if !target_service_ok {
        return false;
    }
    // Method check.
    let (consumer_method, consumer_template) = match &consumer_key.target {
        ConsumerTargetKey::Contract(ContractKey::Http { method, template }) => {
            (method.clone(), Some(template.clone()))
        }
        ConsumerTargetKey::Contract(ContractKey::Topic { .. }) => return false,
        ConsumerTargetKey::Contract(ContractKey::Rpc { .. }) => return false,
        ConsumerTargetKey::UrlExpr(_) => (MethodSpec::Unknown, None),
    };
    let endpoint_method = match &endpoint.1 {
        ContractKey::Http { method, .. } => method.clone(),
        ContractKey::Topic { .. } => return false,
        ContractKey::Rpc { .. } => return false,
    };
    match (consumer_method, endpoint_method) {
        (MethodSpec::Known(cm), MethodSpec::Known(em)) => {
            if cm != em && !matches!(em, crate::federation::contracts::model::HttpMethod::Any) {
                return false;
            }
        }
        (MethodSpec::Unknown, _) | (_, MethodSpec::Unknown) => {}
    }
    // Template check.
    if let Some(template) = consumer_template.as_deref() {
        let endpoint_template = match &endpoint.1 {
            ContractKey::Http { template, .. } => template.clone(),
            _ => return false,
        };
        if !template_matches(template, &endpoint_template) {
            return false;
        }
    }
    // PR 18 — operationId candidate. The URL did not match any
    // endpoint, but the consumer's `via.fn_name` (recorded on the
    // `ConsumerKey.target` as a `UrlExpr`) equals the endpoint's
    // provider's OpenAPI `operationId`. Generated SDK clients
    // (`client.orders.getOrderById({id})`) carry the operationId in
    // their method name; surfacing this candidate lets a tool
    // operator close the gap with a `bindings` entry rather than
    // guessing by URL.
    if let ConsumerTargetKey::UrlExpr(name) = &consumer_key.target {
        if let Some(endpoint_def) = head.endpoints.get(endpoint) {
            if endpoint_def
                .providers
                .iter()
                .any(|p| p.operation_id.as_deref() == Some(name.as_str()))
            {
                return true;
            }
        }
    }
    true
}

/// Local template-match that mirrors §7.4. Returns true on direct
/// match or when prefix stripping finds one.
fn template_matches(consumer: &str, provider: &str) -> bool {
    use crate::federation::contracts::route_match::match_route;
    let outcome = match_route(
        MethodSpec::Unknown,
        consumer,
        crate::federation::contracts::model::HttpMethod::Any,
        provider,
    );
    if outcome.is_match() {
        return true;
    }
    // Prefix tolerance: strip up to 3 leading literal segments from
    // C, one at a time. `split('/')` on `/api/v1/orders/{}` yields
    // `["", "api", "v1", "orders", "{}"]` — skip segment 0 (the empty
    // leading one) and try removing 1..k literal segments, then
    // re-prepend the leading `/`.
    let segs: Vec<&str> = consumer.split('/').collect();
    for k in 1..=3 {
        if k + 1 > segs.len() - 1 {
            break;
        }
        let rest = segs[k + 1..].join("/");
        let stripped = format!("/{rest}");
        let outcome = match_route(
            MethodSpec::Unknown,
            &stripped,
            crate::federation::contracts::model::HttpMethod::Any,
            provider,
        );
        if outcome.is_match() {
            return true;
        }
    }
    false
}

// ─── Coverage builder (§9.7) ─────────────────────────────────────────

/// Build a `Coverage` (§9.7) from a `ContractIndex` + the federation's
/// per-repo states. The `repo_states` argument is the live `live_scope`
/// shape — a `Vec<(repo, RepoCoverage)>` assembled by the caller.
/// `unresolved_consumers` is the list of consumer keys whose
/// resolution is `Unresolved`.
pub fn build_coverage(index: &ContractIndex, repos: Vec<RepoCoverage>, scope: Scope) -> Coverage {
    let unresolved: Vec<ConsumerKey> = index
        .consumers
        .iter()
        .filter_map(|(call_id, resolution)| {
            let key = consumer_key(call_id, resolution);
            match &resolution.target {
                Some(IndexConsumerTarget::Unresolved { .. }) => Some(key),
                _ => None,
            }
        })
        .collect();
    let ambiguous: Vec<ConsumerKey> = index
        .consumers
        .iter()
        .filter_map(|(call_id, resolution)| {
            if resolution.bound_endpoints.len() > 1 {
                let key = consumer_key(call_id, resolution);
                Some(key)
            } else {
                None
            }
        })
        .collect();
    let unnormalized: Vec<ConsumerKey> = index
        .unnormalized
        .iter()
        .filter_map(|call_id| {
            index
                .consumers
                .get(call_id)
                .map(|r| consumer_key(call_id, r))
        })
        .collect();
    let external: Vec<(String, u32)> = {
        let mut v: Vec<(String, u32)> = index
            .external
            .iter()
            .map(|(k, c)| (k.clone(), *c))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    };
    let schemaless_endpoints: Vec<EndpointId> = index
        .endpoints
        .iter()
        .filter(|(_, e)| e.schemas.is_empty() || e.schemas.values().all(|s| s.fields.is_empty()))
        .map(|(id, _)| id.clone())
        .collect();
    Coverage {
        repos,
        unresolved_consumers: unresolved,
        ambiguous,
        unnormalized,
        external,
        stale_bindings: index.stale_bindings.len() as u32,
        schemaless_endpoints,
        complete: scope.unreviewed.is_empty(),
        scope,
        repo_coverages: std::collections::BTreeMap::new(),
    }
}

/// Apply §9.7's `complete` rule. `complete` is true iff
/// `scope.unreviewed` is empty AND (when an endpoint is supplied) no
/// unresolved consumer could match it. The caller passes the index
/// so `could_match` can resolve each consumer's `target_service`
/// (key alone doesn't carry the joiner-known target).
pub fn coverage_complete(
    coverage: &Coverage,
    endpoint: Option<&EndpointId>,
    index: &ContractIndex,
) -> bool {
    if !coverage.scope.unreviewed.is_empty() {
        return false;
    }
    if let Some(target) = endpoint {
        let surface = ContractSurface::from_index(index);
        if coverage
            .unresolved_consumers
            .iter()
            .any(|u| could_match(u, target, &surface))
        {
            return false;
        }
    }
    true
}

// ─── tests ────────────────────────────────────────────────────────────
