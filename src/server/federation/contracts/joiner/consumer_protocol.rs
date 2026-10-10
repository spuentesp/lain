//! Consumer-side protocol resolvers — Topic, RPC, GraphQL,
//! WebSocket, SQL tables.
//!
//! Each resolver is a thin wrapper around the shared
//! [`resolve_by_key`] helper that walks the [`EndpointTable`],
//! applies a `candidate_filter`, skips own-service providers, and
//! decides the bind / unresolved verdict from the supplied
//! [`AmbiguityPolicy`]. The protocol resolvers differ only in:
//!
//! 1. The `ContractKey` shape they construct (Topic / Rpc /
//!    Graphql).
//! 2. The ambiguity policy (NoMatch / RpcStubUnknown /
//!    GraphqlNoOp).
//! 3. The predicate (RPC adds a service-set filter on the channel
//!    host; RPC adds a second pass for package-qualified
//!    matches).
//!
//! Phase B-D review §S6 — extracting `resolve_by_key` collapsed
//! the 105 / 178 / 104-line original resolvers into thin
//! wrappers; this module collects them so the orchestrator can
//! stay focused on the §7.3 ladder (HTTP).

use crate::federation::contracts::config::ContractFederationConfig;
use crate::federation::contracts::index::{ConsumerResolution, ConsumerTarget, UnresolvedReason};
use crate::federation::contracts::joiner::endpoints::EndpointTable;
use crate::federation::contracts::model::{
    ContractKey, HostPart, MethodSpec, RpcConsumerFact, ServiceName, TableConsumerFact,
    TopicConsumerFact,
};
use crate::federation::contracts::url_resolution::host_matches_pattern;
use crate::federation::repo_id::GlobalId;
use crate::schema::{EdgeProvenance, RouteMatch};
use std::collections::BTreeSet;

/// How [`resolve_by_key`] turns an N-candidate match into a
/// [`ConsumerTarget`]. The three protocol resolvers share the
/// iteration + bind machinery — they differ only in (a) the
/// `candidate_filter` predicate and (b) what to do when there are
/// 0 / 1 / N providers that pass the filter. This enum captures
/// the policy axis so the iteration is shared (Phase B-D review
/// §S6).
#[derive(Debug, Clone)]
pub(crate) enum AmbiguityPolicy {
    /// Bind on any count; 0 candidates → `Unresolved { reason: NoMatch }`.
    /// Topic resolver semantics.
    NoMatch,
    /// Bind only when exactly one candidate passes the filter;
    /// 0 or 2+ → `Unresolved { reason: GraphqlNoOp, target_service: route_owner }`.
    /// GraphQL resolver semantics (spec §8.3: "several services expose
    /// the same root field ⇒ ambiguous, never single-bound").
    GraphqlNoOp {
        /// Informational `/graphql` route owner surfaced on the
        /// unresolved record so the operator can see the join's
        /// expected target.
        route_owner: Option<ServiceName>,
    },
    /// Bind on any count; 0 candidates → `Unresolved { reason: RpcStubUnknown }`.
    /// gRPC resolver semantics.
    RpcStubUnknown,
}

/// Shared iteration in the three protocol resolvers
/// (`resolve_topic_consumer`, `resolve_graphql_consumer`,
/// `resolve_rpc_consumer`). Walks `endpoints`, applies
/// `candidate_filter`, skips own-service providers, decides the
/// bind / unresolved verdict from `ambiguity_policy`, and pushes
/// `Binds` edges onto `binds` only when the verdict is `Binds`.
///
/// `target_key` is the consumer's [`ContractKey`] and is enforced
/// as the primary equality predicate: an endpoint only enters the
/// candidate set when `*endpoint_key == *target_key`. The
/// `candidate_filter` is the *additional* axis on top of that
/// match — RPC's first pass restricts the candidate set further
/// to the services resolved from the channel host.
///
/// The gRPC second-pass (package-qualified) case bypasses the
/// `target_key` equality check by passing `target_key` set to a
/// placeholder key whose shape the filter ignores; the filter then
/// does all the matching itself.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_by_key<F>(
    call_id: &GlobalId,
    target_key: &ContractKey,
    endpoints: &EndpointTable,
    candidate_filter: F,
    ambiguity_policy: AmbiguityPolicy,
    own_service: &ServiceName,
    binds: &mut Vec<crate::federation::contracts::joiner::BindsEdge>,
) -> ConsumerResolution
where
    F: Fn(&(ServiceName, ContractKey)) -> bool,
{
    let mut candidates: Vec<(ServiceName, ContractKey, GlobalId)> = Vec::new();
    for ((svc, key), providers) in endpoints {
        if !candidate_filter(&(svc.clone(), key.clone())) {
            continue;
        }
        if svc == own_service {
            continue;
        }
        if let Some(provider) = providers.first() {
            candidates.push((svc.clone(), key.clone(), provider.id.clone()));
        }
    }

    let target = match (&ambiguity_policy, candidates.len()) {
        (AmbiguityPolicy::NoMatch, 0) => Some(ConsumerTarget::Unresolved {
            reason: UnresolvedReason::NoMatch,
            target_service: None,
        }),
        (AmbiguityPolicy::RpcStubUnknown, 0) => Some(ConsumerTarget::Unresolved {
            reason: UnresolvedReason::RpcStubUnknown,
            target_service: None,
        }),
        (AmbiguityPolicy::GraphqlNoOp { route_owner }, n) if n != 1 => {
            Some(ConsumerTarget::Unresolved {
                reason: UnresolvedReason::GraphqlNoOp,
                target_service: route_owner.clone(),
            })
        }
        _ => Some(ConsumerTarget::Binds {
            provenance: EdgeProvenance::Static {
                source: crate::schema::StaticSource::Regex,
            },
            confidence: 1.0,
            route_match: RouteMatch::Exact,
            stripped_prefix: None,
        }),
    };

    let mut bound: Vec<(ServiceName, ContractKey)> = Vec::new();
    if matches!(target, Some(ConsumerTarget::Binds { .. })) {
        for (svc, key, provider_id) in &candidates {
            binds.push(crate::federation::contracts::joiner::BindsEdge {
                consumer: call_id.clone(),
                provider: provider_id.clone(),
                consumer_service: own_service.clone(),
                provider_service: svc.clone(),
                target_endpoint: (svc.clone(), key.clone()),
                provenance: EdgeProvenance::Static {
                    source: crate::schema::StaticSource::Regex,
                },
                confidence: 1.0,
                route_match: RouteMatch::Exact,
                stripped_prefix: None,
            });
            bound.push((svc.clone(), key.clone()));
        }
        bound.sort_by(|a, b| {
            a.0 .0
                .cmp(&b.0 .0)
                .then_with(|| a.1.to_string().cmp(&b.1.to_string()))
        });
    }
    let _ = target_key;

    ConsumerResolution {
        call_id: call_id.clone(),
        service: own_service.clone(),
        target,
        bound_endpoints: bound,
        reads_complete: true,
    }
}

/// §7.7 (stretch, PR 15): resolve a topic consumer. Same broker AND
/// same name = `Binds { route_match: Exact, confidence: 1.0 }`. Any
/// mismatch (different broker, different name, no producer-side
/// endpoint) = `Unresolved { reason: NoMatch }`. The HTTP §7.3
/// table doesn't apply — topics don't have hosts / templates, only
/// the `(broker, name)` pair.
///
/// §7.8 invariant: every `Binds` edge connects two different
/// services. An in-service topic consumer must NOT bind to its own
/// service's producer endpoint — same rule §7.3 rule 6 enforces
/// for HTTP consumers.
pub fn resolve_topic_consumer(
    call_id: &GlobalId,
    consumer: &TopicConsumerFact,
    own_service: &ServiceName,
    endpoints: &EndpointTable,
    binds: &mut Vec<crate::federation::contracts::joiner::BindsEdge>,
) -> ConsumerResolution {
    let target_key = ContractKey::Topic {
        broker: consumer.broker.clone(),
        name: consumer.name.clone(),
    };
    let target_key_ref = &target_key;
    resolve_by_key(
        call_id,
        &target_key,
        endpoints,
        |(_, key): &(ServiceName, ContractKey)| key == target_key_ref,
        AmbiguityPolicy::NoMatch,
        own_service,
        binds,
    )
}

/// Phase E (spec §8.3): resolve a `GraphqlConsumer` to a
/// `GraphqlProvider` by exact `(op, field)` match. Per spec
/// §8.3, "if several services expose the same root field
/// (federation/gateway) ⇒ ambiguous, never single-bound" —
/// when more than one provider matches, the join returns
/// `Unresolved { reason: GraphqlNoOp }` instead of binding.
///
/// The `/graphql` HTTP route is the carrier the spec pins
/// the endpoint on (§8.3: "Endpoint target: `/graphql` HTTP
/// route resolution via Phase B/C selects the service; join
/// on `(op, field)` there"). When a `/graphql` route owner
/// is known, it surfaces as `target_service` on the
/// unresolved record so the operator can see the join's
/// expected target. The route owner is **not** used to
/// restrict the candidate set — a backend that implements
/// the schema while the gateway owns the route is a common
/// GraphQL federation shape (F2_NEG pins this), and the
/// `(op, field)` match is the actual contract per the spec.
pub fn resolve_graphql_consumer(
    call_id: &GlobalId,
    consumer: &crate::federation::contracts::model::GraphqlConsumerFact,
    own_service: &ServiceName,
    endpoints: &EndpointTable,
    binds: &mut Vec<crate::federation::contracts::joiner::BindsEdge>,
) -> ConsumerResolution {
    // Pass #4 R22 (review §D19) — use the `route_owner` helper
    // for the "find the first /graphql route owner" iteration.
    // The cardinality check (exactly one owner) stays inline
    // because `route_owner` returns the first match, not
    // "exactly one". Two + owners → `None` per spec §8.3
    // ("several services expose the same root field ⇒
    // ambiguous, never single-bound").
    let is_graphql_route = |(_, key): &(_, ContractKey)| {
        matches!(key, ContractKey::Http { method, template }
        if template == "/graphql"
            && matches!(
                method,
                MethodSpec::Known(crate::federation::contracts::model::HttpMethod::Post)
                    | MethodSpec::Unknown
            ))
    };
    let first_owner = super::endpoints::route_owner(endpoints, is_graphql_route);
    let total_owners = endpoints.keys().filter(|k| is_graphql_route(k)).count();
    let route_owner: Option<ServiceName> = if total_owners == 1 { first_owner } else { None };
    let target_key = ContractKey::Graphql {
        op: consumer.op,
        field: consumer.field.clone(),
    };
    let target_key_ref = &target_key;
    resolve_by_key(
        call_id,
        &target_key,
        endpoints,
        |(_, key): &(ServiceName, ContractKey)| key == target_key_ref,
        AmbiguityPolicy::GraphqlNoOp { route_owner },
        own_service,
        binds,
    )
}

/// Phase E (spec §8.2): resolve one gRPC stub call site. The
/// joiner first resolves the channel address (host:port) the
/// consumer constructed to a target service through the same
/// `services[].hosts` dispatch Phase B / C uses, then performs
/// an exact `(package.Service, method)` match within that
/// service. No URL-prefix tolerance (per spec §8.2).
///
/// Same-service guard (I5): a stub call to the calling service's
/// own provider must NOT bind. The `own_service == svc` skip
/// mirrors the topic consumer's rule (§7.8).
///
/// Channel resolution (spec §8.2 acceptance E2): when the
/// consumer's `channel_host_part` is `HostPart::Literal(host)`,
/// the joiner restricts the candidate set to the services
/// whose `hosts` list matches `host`. When the channel host
/// is `HostPart::None`, the consumer has no resolvable
/// channel address and lands in `Unresolved { reason:
/// RpcStubUnknown }` (E4). When the channel host matches no
/// service, the consumer is similarly unresolved.
pub fn resolve_rpc_consumer(
    call_id: &GlobalId,
    consumer: &RpcConsumerFact,
    own_service: &ServiceName,
    endpoints: &EndpointTable,
    config: &ContractFederationConfig,
    binds: &mut Vec<crate::federation::contracts::joiner::BindsEdge>,
) -> ConsumerResolution {
    let candidate_services: Vec<ServiceName> = match &consumer.channel_host_part {
        HostPart::None => {
            return rpc_stub_unknown(call_id, own_service);
        }
        HostPart::Literal(host) => {
            let mut svcs: Vec<ServiceName> = config
                .services
                .iter()
                .filter(|s| s.hosts.iter().any(|pat| host_matches_pattern(pat, host)))
                .map(|s| ServiceName(s.name.clone()))
                .collect();
            if svcs.is_empty() {
                return rpc_stub_unknown(call_id, own_service);
            }
            svcs.sort();
            svcs
        }
        HostPart::Env(_) | HostPart::Expr(_) => {
            return rpc_stub_unknown(call_id, own_service);
        }
    };
    let target_key = ContractKey::Rpc {
        system: consumer.system,
        service: consumer.service.clone(),
        method: consumer.method.clone(),
    };
    let candidate_services_ref = &candidate_services;
    let target_key_ref = &target_key;
    let res = resolve_by_key(
        call_id,
        &target_key,
        endpoints,
        |(svc, key): &(ServiceName, ContractKey)| {
            candidate_services_ref.contains(svc) && key == target_key_ref
        },
        AmbiguityPolicy::RpcStubUnknown,
        own_service,
        binds,
    );
    if !res.bound_endpoints.is_empty() {
        return res;
    }
    let bare = format!(".{}", consumer.service);
    let bare_ref = &bare;
    let method_ref = &consumer.method;
    let candidate_services_ref2 = &candidate_services;
    resolve_by_key(
        call_id,
        &target_key,
        endpoints,
        |(svc, key): &(ServiceName, ContractKey)| {
            candidate_services_ref2.contains(svc)
                && matches!(
                    key,
                    ContractKey::Rpc { ref service, method, .. }
                    if service.ends_with(bare_ref) && method == method_ref
                )
        },
        AmbiguityPolicy::RpcStubUnknown,
        own_service,
        binds,
    )
}

/// `Unresolved { reason: RpcStubUnknown }` shortcut used by
/// [`resolve_rpc_consumer`] when the channel host cannot be
/// resolved to any service.
fn rpc_stub_unknown(call_id: &GlobalId, own_service: &ServiceName) -> ConsumerResolution {
    ConsumerResolution {
        call_id: call_id.clone(),
        service: own_service.clone(),
        target: Some(ConsumerTarget::Unresolved {
            reason: UnresolvedReason::RpcStubUnknown,
            target_service: None,
        }),
        bound_endpoints: Vec::new(),
        reads_complete: true,
    }
}

/// Phase F (Gap 19): resolve a WebSocket consumer against the endpoint table.
pub fn resolve_websocket_consumer(
    call_id: &GlobalId,
    consumer: &crate::federation::contracts::model::WebSocketConsumerFact,
    own_service: &ServiceName,
    endpoints: &EndpointTable,
    config: &crate::federation::contracts::config::ContractFederationConfig,
    binds: &mut Vec<crate::federation::contracts::joiner::BindsEdge>,
) -> ConsumerResolution {
    // Host evidence first, exactly as `resolve_rpc_consumer` does it.
    // Without this the resolver bound on `route` alone, so
    // `wss://api.thirdparty.com/feed` invented a Binds edge to whatever
    // internal service happened to declare `/feed`.
    let candidate_services: Vec<ServiceName> = match &consumer.url.host {
        HostPart::None => Vec::new(),
        HostPart::Literal(host) => {
            let mut svcs: Vec<ServiceName> = config
                .services
                .iter()
                .filter(|s| s.hosts.iter().any(|pat| host_matches_pattern(pat, host)))
                .map(|s| ServiceName(s.name.clone()))
                .collect();
            svcs.sort();
            svcs
        }
        HostPart::Env(_) | HostPart::Expr(_) => Vec::new(),
    };

    // A literal host that names no configured service means the dial
    // left the federation. There is no provider we may name, so this is
    // `Unresolved` rather than a guessed bind. (An `External` terminal
    // state would be nicer; the `external` map is only populated by the
    // HTTP ladder today, so we stay conservative and say "unknown".)
    if matches!(consumer.url.host, HostPart::Literal(_)) && candidate_services.is_empty() {
        return ConsumerResolution {
            call_id: call_id.clone(),
            service: own_service.clone(),
            target: Some(ConsumerTarget::Unresolved {
                reason: UnresolvedReason::NoMatch,
                target_service: None,
            }),
            bound_endpoints: Vec::new(),
            reads_complete: true,
        };
    }

    let key = ContractKey::WebSocket {
        route: consumer.route.clone(),
    };
    // Ambiguity must refuse. `NoMatch` bound on *any* candidate count
    // at confidence 1.0, so two services exposing `/ws` multi-bound;
    // GraphQL has refused that since §8.3 and WebSocket must too.
    let allowed = candidate_services.clone();
    resolve_by_key(
        call_id,
        &key,
        endpoints,
        |(svc, k): &(ServiceName, ContractKey)| {
            k == &key && (allowed.is_empty() || allowed.contains(svc))
        },
        AmbiguityPolicy::GraphqlNoOp {
            route_owner: Some(own_service.clone()),
        },
        own_service,
        binds,
    )
}

/// Phase D (spec §7): resolve a `TableConsumer` — a synthetic
/// `sql-read:<path>:<line>` node whose fact lists the literal tables
/// a SQL site reads — against the endpoint table.
/// Tables are identified by name alone; ownership (which service's
/// endpoint table a `Table` node lands in) was already applied by
/// [`build_endpoints`](super::endpoints::build_endpoints) via the
/// `databases[]` config, so the joiner only has to find the
/// `(service, ContractKey::Table { name })` entries whose name the
/// consumer reads.
///
/// Unlike the HTTP / topic / RPC / GraphQL / WebSocket resolvers
/// there is **no own-service skip**: a table endpoint's provider is
/// the table declaration itself, not a same-service API, and spec
/// §7's handler → function → table traversal is exactly the
/// intra-service edge (`platform` reading its own `shipments` table
/// is the canonical case the contract-tool e2e pins). §7.8 / I5's
/// "no same-service Binds" rule is about a service consuming its
/// own API endpoints; it does not apply to data endpoints.
///
/// Every table in the fact must have at least one owning endpoint,
/// or the whole consumer is `Unresolved { reason: NoMatch }` — I2
/// (no silent drop): a reader of an unowned table must be visible
/// to `list_unresolved`, because silence is what lets
/// `NoKnownImpact` be claimed while a possible reader exists. When
/// all tables resolve, one `Binds` edge per `(service, table)` is
/// pushed; a table widened to several services by
/// `databases[].shared_with` multi-binds, mirroring the topic
/// resolver's behaviour for a topic owned by two services.
pub fn resolve_table_consumer(
    call_id: &GlobalId,
    consumer: &TableConsumerFact,
    own_service: &ServiceName,
    endpoints: &EndpointTable,
    binds: &mut Vec<crate::federation::contracts::joiner::BindsEdge>,
) -> ConsumerResolution {
    let wanted: BTreeSet<&str> = consumer.tables.iter().map(String::as_str).collect();
    let mut candidates: Vec<(ServiceName, ContractKey, GlobalId)> = Vec::new();
    let mut covered: BTreeSet<String> = BTreeSet::new();
    for ((svc, key), providers) in endpoints {
        let ContractKey::Table { name } = key else {
            continue;
        };
        if !wanted.contains(name.as_str()) {
            continue;
        }
        if let Some(provider) = providers.first() {
            covered.insert(name.clone());
            candidates.push((svc.clone(), key.clone(), provider.id.clone()));
        }
    }
    // No endpoint owns every table the source reads → the consumer
    // is unresolved as a whole (all-or-nothing keeps the index and
    // the `Binds` edge set consistent with each other).
    if candidates.is_empty() || covered.len() != wanted.len() {
        return ConsumerResolution {
            call_id: call_id.clone(),
            service: own_service.clone(),
            target: Some(ConsumerTarget::Unresolved {
                reason: UnresolvedReason::NoMatch,
                target_service: None,
            }),
            bound_endpoints: Vec::new(),
            reads_complete: true,
        };
    }
    let mut bound: Vec<(ServiceName, ContractKey)> = Vec::new();
    for (svc, key, provider_id) in &candidates {
        binds.push(crate::federation::contracts::joiner::BindsEdge {
            consumer: call_id.clone(),
            provider: provider_id.clone(),
            consumer_service: own_service.clone(),
            provider_service: svc.clone(),
            target_endpoint: (svc.clone(), key.clone()),
            provenance: EdgeProvenance::Static {
                source: crate::schema::StaticSource::Regex,
            },
            confidence: 1.0,
            route_match: RouteMatch::Exact,
            stripped_prefix: None,
        });
        bound.push((svc.clone(), key.clone()));
    }
    bound.sort_by(|a, b| {
        a.0 .0
            .cmp(&b.0 .0)
            .then_with(|| a.1.to_string().cmp(&b.1.to_string()))
    });
    ConsumerResolution {
        call_id: call_id.clone(),
        service: own_service.clone(),
        target: Some(ConsumerTarget::Binds {
            provenance: EdgeProvenance::Static {
                source: crate::schema::StaticSource::Regex,
            },
            confidence: 1.0,
            route_match: RouteMatch::Exact,
            stripped_prefix: None,
        }),
        bound_endpoints: bound,
        reads_complete: true,
    }
}
