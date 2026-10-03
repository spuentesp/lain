//! Per-federation `ContractIndex` (`docs/CONTRACT_FEDERATION.md` §4.3).
//!
//! The index is derived by `FederatedIndex::rejoin_contracts` (PR 7)
//! and never persisted. It carries every fact a contract tool needs
//! that does not belong in the `Binds` edge table: services,
//! endpoints, consumer resolutions, field-ref resolutions,
//! stale confirmed bindings, the external-host tally, and the list
//! of unnormalized calls.
//!
//! All collections are `BTreeMap` / `Vec` sorted by their key — the
//! joiner relies on the deterministic ordering for its purity
//! invariant (§7.8): same input, same output, same order.

use std::collections::BTreeMap;

use crate::federation::contracts::config::RoutePrefix;
use crate::federation::contracts::model::{
    ContractFact, ContractKey, Direction, FieldMeta, JsonPath, ServiceName,
};
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::schema::{EdgeProvenance, NodeType, RouteMatch};

/// The per-federation contract index. Derived by `rejoin_contracts`,
/// never persisted; tools (PR 10+) read this snapshot.
///
/// The shape is fixed by §4.3. Adding a field is a wire-shape change
/// for the tools that serialize it; see PR 13 / 16 for the consumer
/// story.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContractIndex {
    pub services: BTreeMap<ServiceName, ServiceInfo>,
    pub endpoints: BTreeMap<EndpointId, Endpoint>,
    pub consumers: BTreeMap<GlobalId, ConsumerResolution>,
    pub field_refs: BTreeMap<GlobalId, FieldRefResolution>,
    pub stale_bindings: Vec<StaleBinding>,
    pub external: BTreeMap<String, u32>,
    pub unnormalized: Vec<GlobalId>,
}

/// `(ServiceName, ContractKey)` — the unique identifier of an
/// endpoint in a federation. The joiner indexes endpoints by this;
/// tools serialize it as two fields in every payload (§4.4).
pub type EndpointId = (ServiceName, ContractKey);

/// One declared service, after §7.1 validation. `endpoint_ids` is
/// filled by the joiner so tools can list a service's endpoints
/// without scanning the federation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInfo {
    pub name: ServiceName,
    pub repo: RepoId,
    pub paths: Vec<String>,
    pub hosts: Vec<String>,
    pub env: Vec<String>,
    pub base_path: Option<String>,
    pub route_prefixes: Vec<RoutePrefix>,
    /// Endpoint ids owned by this service. Sorted.
    pub endpoint_ids: Vec<EndpointId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub id: EndpointId,
    pub method: HttpMethodConst,
    pub template: String,
    pub providers: Vec<EndpointProvider>,
    /// Response / request / payload schemas. Keyed by direction.
    pub schemas: BTreeMap<Direction, EndpointSchema>,
}

/// `HttpMethod` is its own enum, but endpoints carry the consumer
/// view too (`MethodSpec::Unknown`); we keep `Endpoint::method` as
/// the more flexible `MethodSpec` so the joiner's ambiguous cases
/// (rule 6 multi-match) and confirmed bindings (rule 2) can produce
/// endpoints that match consumer keys exactly. Provider methods
/// never carry `Unknown` (sensors emit `HttpMethod::Any` instead).
pub type HttpMethodConst = crate::federation::contracts::model::HttpMethod;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointProvider {
    pub node_id: GlobalId,
    pub origin: ProviderOriginConst,
    pub handler: Option<crate::federation::contracts::model::SymbolKey>,
    pub operation_id: Option<String>,
}

pub type ProviderOriginConst = crate::federation::contracts::model::ProviderOrigin;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointSchema {
    pub node_id: GlobalId,
    pub fields: BTreeMap<JsonPath, FieldMeta>,
}

/// What the joiner decided for one `HttpClientCall`. The
/// `bound_endpoints` list is empty when the call is unresolved,
/// external, or unnormalized — the union of the three states is
/// recorded in `unresolved` / `target` so a tool can render the
/// reason.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsumerResolution {
    pub call_id: GlobalId,
    pub service: ServiceName,
    pub target: Option<ConsumerTarget>,
    pub bound_endpoints: Vec<EndpointId>,
    pub reads_complete: bool,
}

/// The way the joiner chose to bind a call. `External` means rule 4;
/// `Unresolved` means rule 3 / rule 6 found no match; `Binds`
/// matches rule 3 / rule 6 hits. Confirmed bindings (rule 2) are
/// also `Binds` with `provenance = Confirmed`.
#[derive(Debug, Clone, PartialEq)]
pub enum ConsumerTarget {
    Binds {
        provenance: EdgeProvenance,
        confidence: f32,
        route_match: RouteMatch,
        stripped_prefix: Option<String>,
    },
    External {
        host: String,
    },
    Unresolved {
        reason: UnresolvedReason,
        /// `Some(s)` when the joiner knew the target service but
        /// found no matching route (§7.3 rule 3 —
        /// `NoRouteInService`). `None` when the target service is
        /// unknown (§7.3 rule 6 — `NoMatch`). §9.7's could-match
        /// rule uses this to gate unresolved candidates: a
        /// `Some(s)` unresolved only matches endpoints whose
        /// service is `s`; `None` matches anything. The field is
        /// `Option` because `UnresolvedReason::Unnormalized`
        /// (§7.3 rule 5) has no resolved target either.
        target_service: Option<ServiceName>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnresolvedReason {
    /// Rule 3: target service known but no route in it.
    NoRouteInService,
    /// Rule 6: target unknown, no service matches.
    NoMatch,
    /// Rule 5: template was dynamic.
    Unnormalized,
    /// Phase A rule-1 fix: a `CallVia::Receiver` with no matching
    /// `http_clients` entry. Pre-Phase-A, these were silently dropped
    /// (a soundness bug — the consumer was neither bound nor
    /// recorded). Phase A records them as Unresolved so the
    /// coverage ledger and the verdict downgrade see them.
    WrapperUnconfigured,
    /// Phase C: the consumer's `HostPart::Env([var])` referenced a
    /// var the env_sensor has no binding for. The var is also
    /// recorded in the coverage ledger (`unresolved` bucket with
    /// `reason: EnvUnmapped`) so the operator can wire a value.
    EnvUnmapped,
    /// Phase C: the consumer's `HostPart::Env([var])` resolved to
    /// multiple distinct hosts in the env_sensor's bindings
    /// (`.env` says one thing, compose says another). No bind
    /// emitted; the consumer is ambiguous.
    EnvAmbiguous,
    /// Phase E (spec §8.2): the consumer's gRPC stub call
    /// resolved to a known service but the channel address
    /// (host:port) the consumer constructed has no matching
    /// service in `services[].hosts`. The consumer lands in
    /// the coverage ledger's `unresolved` bucket; the operator
    /// wires the host via `repos.yaml#services[]`.
    RpcStubUnknown,
    /// Phase E (spec §8.3): a GraphQL consumer whose
    /// `(op, field)` did not match any provider scoped to
    /// the service that owns the `/graphql` HTTP route. The
    /// consumer lands in the coverage ledger's `unresolved`
    /// bucket with this reason so the operator can wire a
    /// missing schema field or expand the route's
    /// service-scoped provider set.
    GraphqlNoOp,
}

/// Field read resolution (§7.5). `bound_fields` carries the joined
/// `Binds(FieldRef → Field)` product; `call` is the id of the
/// `HttpClientCall` the FieldRef `ReadsFrom` (empty when the
/// FieldRef is orphaned). The index carries the link so tools and
/// `ContractSurface::from_index` can rebuild each consumer's
/// `reads` set without re-reading graph edges (§4.3: "everything
/// else the tools need lives in `ContractIndex`").
#[derive(Debug, Clone, PartialEq)]
pub struct FieldRefResolution {
    pub field_ref_id: GlobalId,
    pub service: ServiceName,
    pub bound_fields: Vec<BoundField>,
    pub unknown: bool,
    /// The `ReadsFrom` target: the `HttpClientCall` GlobalId this
    /// read belongs to. Empty when no `ReadsFrom` edge exists.
    pub call: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoundField {
    pub field: GlobalId,
    pub field_path: JsonPath,
    pub endpoint: EndpointId,
    pub confidence: f32,
}

/// A confirmed-binding entry that matched no consumer
/// (`reason = NoConsumer`) or whose endpoint does not exist
/// (`reason = NoEndpoint`). Keyed by `(repo, path, symbol, key)` per
/// §7.6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleBinding {
    pub consumer: ConfirmedBindingKey,
    pub provider: ConfirmedBindingProviderKey,
    pub reason: StaleReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedBindingKey {
    pub repo: RepoId,
    pub path: String,
    pub symbol: String,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedBindingProviderKey {
    pub service: ServiceName,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleReason {
    NoConsumer,
    NoEndpoint,
}

/// Re-export of the schema's `NodeType` so callers that hold a
/// `ContractIndex` do not have to thread the schema module.
pub fn node_type_of(fact: &ContractFact) -> NodeType {
    match fact {
        ContractFact::Provider(_) => NodeType::HttpRoute,
        ContractFact::Consumer(_) => NodeType::HttpClientCall,
        ContractFact::Schema { .. } => NodeType::Schema,
        ContractFact::Field(_) => NodeType::Field,
        ContractFact::FieldRead(_) => NodeType::FieldRef,
        // §6.7 (stretch): topic subscribers ride on `TopicConsumer`.
        // We surface them as a fresh node type so tools can ask the
        // graph "which functions subscribe to which topics?" without
        // parsing edges. The schema carries `NodeType::Topic` for
        // the producer-side; consumers are still function-shaped.
        ContractFact::TopicConsumer(_) => NodeType::Function,
        // Phase D (spec §7): a `Table` contract payload maps to a
        // `Table` node. The payload carries `(service, name)`; the
        // joiner fills `service` later, so an empty `service` at
        // scan time is fine — the node is still keyed on `name`.
        ContractFact::Table(_) => NodeType::Table,
        // Phase E (spec §8.2): `RpcProvider` rides on a Module
        // node (the proto file's path-keyed module); the
        // `RpcHandler` link is keyed off the handler function.
        // Both surface as `Module` today so the existing typed
        // traversal still works; a future PR can introduce a
        // dedicated `RpcService` node type if a tool needs to
        // filter for them.
        ContractFact::RpcProvider(_) | ContractFact::RpcHandler(_) => NodeType::Module,
        // Phase E (spec §8.2): `RpcConsumer` rides on a function
        // / method / file (mirrors how `SendsHttp` attaches to
        // the enclosing function). Function is the primary
        // shape.
        ContractFact::RpcConsumer(_) => NodeType::Function,
        // Phase E (spec §8.3): `GraphqlProvider` rides on a
        // Module node (the SDL file's path-keyed module). The
        // `GraphqlHandler` link is keyed off the handler
        // function. Both surface as `Module` so the existing
        // typed traversal works.
        ContractFact::GraphqlProvider(_) | ContractFact::GraphqlHandler(_) => NodeType::Module,
        // Phase E (spec §8.3): `GraphqlConsumer` rides on a
        // function / method (mirrors how `SendsHttp` attaches
        // to the enclosing function). Function is the primary
        // shape.
        ContractFact::GraphqlConsumer(_) => NodeType::Function,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_id_is_hashable_and_orderable() {
        use crate::federation::contracts::model::{HttpMethod, MethodSpec};
        let a: EndpointId = (
            ServiceName("orders".into()),
            ContractKey::Http {
                method: MethodSpec::Known(HttpMethod::Get),
                template: "/api/orders".into(),
            },
        );
        let b: EndpointId = (
            ServiceName("billing".into()),
            ContractKey::Http {
                method: MethodSpec::Known(HttpMethod::Post),
                template: "/api/billing".into(),
            },
        );
        let mut map: BTreeMap<EndpointId, u32> = BTreeMap::new();
        map.insert(a.clone(), 1);
        map.insert(b.clone(), 2);
        assert_eq!(map.len(), 2);
        assert_eq!(map.get(&a), Some(&1));
        assert_eq!(map.get(&b), Some(&2));
    }

    #[test]
    fn node_type_of_maps_each_contract_fact_variant() {
        let repo = RepoId::new("orders").unwrap();
        use crate::federation::contracts::model::*;
        let p = ContractFact::Provider(ProviderFact {
            method: HttpMethod::Get,
            template: "/".into(),
            handler: None,
            operation_id: None,
            origin: ProviderOrigin::Code,
        });
        assert_eq!(node_type_of(&p), NodeType::HttpRoute);
        let c = ContractFact::Consumer(ConsumerFact {
            method: MethodSpec::Known(HttpMethod::Get),
            url: NormalizedUrl {
                host: HostPart::None,
                template: Some("/".into()),
            },
            via: CallVia::Library { name: "x".into() },
            url_expr: "x".into(),
            reads_complete: true,
        });
        assert_eq!(node_type_of(&c), NodeType::HttpClientCall);
        let s = ContractFact::Schema {
            direction: Direction::Response,
        };
        assert_eq!(node_type_of(&s), NodeType::Schema);
        let f = ContractFact::Field(FieldMeta {
            ty: TypeDesc::String,
            required: true,
            nullable: false,
            enum_values: None,
        });
        assert_eq!(node_type_of(&f), NodeType::Field);
        let fr = ContractFact::FieldRead(FieldReadFact {
            chain: JsonPath(vec![PathSegment::Name("x".into())]),
            exact: true,
        });
        assert_eq!(node_type_of(&fr), NodeType::FieldRef);
        // `repo` used to silence the unused warning.
        let _ = repo;
    }
}
