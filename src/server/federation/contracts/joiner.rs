//! Contract joiner (`docs/CONTRACT_FEDERATION.md` §5.3 + §7.2 / §7.3 /
//! §7.4 / §7.5 / §7.6).
//!
//! `ContractJoiner::run(nodes, edges, config) -> JoinOutput` is the
//! pure inner half of the federation's contract join. The caller
//! supplies the projected contract nodes, the projected contract
//! edges, and the validated config; the joiner returns the desired
//! `Binds` set and the `ContractIndex`. No I/O, no reads from the
//! graph, no config reads inside `run`.
//!
//! PR 7 left step 5 (§7.5 field join) as a documented no-op because
//! no `FieldRef` nodes existed yet. PR 9 fills it: the joiner now
//! collects response schemas per endpoint (from `HasField` /
//! `RequestSchema` / `ResponseSchema` edges) and resolves every
//! `FieldRef` against the endpoints its `HttpClientCall` is bound to.

use std::collections::{BTreeMap, BTreeSet};

use crate::federation::contracts::clients::{ClientDef, ClientRegistry};
use crate::federation::contracts::config::{
    ConfirmedBinding, ContractFederationConfig, RoutePrefix as CfgRoutePrefix, ServiceDecl,
};
use crate::federation::contracts::field_join::{
    collect_endpoint_schemas, resolve_field_refs, EndpointSchemas,
};
use crate::federation::contracts::index::{
    ConfirmedBindingKey, ConfirmedBindingProviderKey, ConsumerResolution, ConsumerTarget,
    ContractIndex, Endpoint, EndpointId, EndpointProvider, ServiceInfo, StaleBinding, StaleReason,
    UnresolvedReason,
};
use crate::federation::contracts::model::{
    CallVia, ConsumerFact, ContractFact, ContractKey, Direction, HostPart, HttpMethod, MethodSpec,
    ProviderFact, ProviderOrigin, RpcConsumerFact, ServiceName, TopicConsumerFact,
};
use crate::federation::contracts::protocol_dispatch::{default_dispatch_chain, ProtocolDispatch};
use crate::federation::contracts::route_match::{
    compare_specificity, match_route, MatchDetail, MatchOutcome,
};
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::schema::{EdgeProvenance, GraphEdge, GraphNode, RouteMatch};
use crate::server::sensors::env_sensor::EnvBindingIndex;

/// Spec §5.3 — I6 total-order resolution precedence, condensed as a
/// Rust enum for the joiner to surface at the per-call layer.
///
/// The full order (highest first):
///
/// 1. Confirmed binding (apply_confirmed_binding, §7.6).
/// 2. Code-derived base + host/env (Phase B [`ClientRegistry`] —
///    composes `base ++ call_path`).
/// 3. `http_clients` config pattern (§7.3 rule 3 first branch).
/// 4. operationId match (Heuristic 0.9) — falls back when URL
///    doesn't match a known target service.
/// 5. Unbound-host heuristic (0.6) / ambiguous (0.3) (§7.3 rule 6).
/// 6. Unresolved candidate (reason recorded).
///
/// **Total-order property (I6):** once a tier binds, no lower tier
/// can override the bind. This is the joiner's central invariant —
/// see `resolve_consumer_to_service` below for the function that
/// walks the tiers in spec order and returns at the first hit.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// Tier 1 (Confirmed) or tier 2/3 with a known target service +
    /// a route that matches. Carries the originating `ServiceName`
    /// and the matching `EndpointId` (None for Confirmed bindings
    /// that don't go through the route table).
    Endpoint {
        service: ServiceName,
        endpoint: EndpointId,
        provenance: EdgeProvenance,
        confidence: f32,
        route_match: RouteMatch,
        stripped_prefix: Option<String>,
    },
    /// Tier 4 (rule 4 in §7.3): a `HostPart::Literal` that matches
    /// no service and isn't on the exempt list. Recorded in
    /// `external`; no `Binds` edge emitted.
    External { host: String },
    /// Tier 5/6: no bindable target — `Unresolved` with a reason.
    /// `target_service` carries the known service when rule 3 found
    /// one but rule 4 couldn't match a route (§7.3 row 3's
    /// `NoRouteInService`).
    Unresolved {
        reason: UnresolvedReason,
        target_service: Option<ServiceName>,
    },
}

/// The output of a `ContractJoiner::run` call. The federation-level
/// orchestrator (`FederatedIndex::rejoin_contracts`) diffs the
/// `binds` set against the current `Binds` edges in the graph backend
/// and applies adds/removes through `upsert_edges_batch` +
/// `remove_edges`. The `index` is stored on a `RwLock<...>` for tools.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JoinOutput {
    pub binds: Vec<BindsEdge>,
    pub index: ContractIndex,
    /// Phase C (spec §6): env vars the joiner tried to resolve via
    /// the env_sensor's bindings but had no mapping for. Keyed by
    /// var name; the count is the number of consumers that
    /// referenced the var. The orchestrator folds these into the
    /// coverage ledger as `unresolved` records with
    /// `reason: EnvUnmapped`.
    pub unresolved_env_vars: BTreeMap<String, u32>,
    /// Phase C (spec §6): env vars the joiner found to resolve to
    /// multiple distinct hosts in the env_sensor's bindings
    /// (`.env` says one thing, compose says another). The consumer
    /// is recorded as `Unresolved { reason: EnvAmbiguous }` and no
    /// `Binds` edge is emitted. The list of distinct hosts is kept
    /// here so the operator can disambiguate.
    pub ambiguous_env_vars: BTreeMap<String, Vec<String>>,
}

/// One desired `Binds` edge, with the join details carried on it so
/// the graph writer can set `GraphEdge.detail` and `provenance`
/// correctly. Sorted by `(consumer, provider)`.
#[derive(Debug, Clone, PartialEq)]
pub struct BindsEdge {
    pub consumer: GlobalId,
    pub provider: GlobalId,
    pub consumer_service: ServiceName,
    pub provider_service: ServiceName,
    pub target_endpoint: EndpointId,
    pub provenance: EdgeProvenance,
    pub confidence: f32,
    pub route_match: RouteMatch,
    pub stripped_prefix: Option<String>,
}

/// Confidence assigned to an `operationId`-only bind (PR 18).
///
/// Higher than prefix-stripped (0.5) and unbound-host (0.6); lower
/// than the rule-3 URL match (Static 1.0). OperationIds are unique
/// within an OpenAPI spec, so the name match is strong evidence, but
/// the URL does not actually match, so it cannot be `Static`. Per
/// §4.6, a `Heuristic` provenance does not count as "certain" for
/// `Verified` — keeping room for a `NeedsInvestigation` verdict when
/// the bind is used in a critical change.
const OPERATION_ID_CONFIDENCE: f32 = 0.9;

/// The joiner.
pub struct ContractJoiner;

impl ContractJoiner {
    /// Compute the desired `Binds` set and the `ContractIndex` from
    /// `nodes`, `edges`, and `config`. Pure function of its inputs
    /// (§7.8).
    ///
    /// `nodes` is the slice of every contract-bearing node the
    /// federation has projected; the orchestrator pre-filters the
    /// backend list to `contract_node_ids` (per repo) to avoid a
    /// whole-graph scan (§5.3 Cost). `edges` is the matching slice
    /// of `ReadsField`, `ReadsFrom`, `HasField`, `RequestSchema`, and
    /// `ResponseSchema` edges — the joiner reads only these to fill
    /// the §7.5 step. Other edge types in `edges` are ignored.
    pub fn run(
        nodes: &[GraphNode],
        edges: &[GraphEdge],
        config: &ContractFederationConfig,
    ) -> JoinOutput {
        Self::run_with_registry(nodes, edges, config, &ClientRegistry::new())
    }

    /// Phase B (spec §5.1 / §5.3 tier 2): same as [`run`] but the
    /// caller passes the per-repo client registry the cross-file
    /// pre-pass built. An empty registry is equivalent to [`run`]
    /// (no tier-2 resolution). The orchestrator
    /// (`FederatedIndex::rejoin_contracts`) is the primary caller;
    /// the property tests in `tests/property_join_pipeline.rs` and
    /// the acceptance tests in `tests/wrapper_resolution.rs` use
    /// this surface directly.
    pub fn run_with_registry(
        nodes: &[GraphNode],
        edges: &[GraphEdge],
        config: &ContractFederationConfig,
        registry: &ClientRegistry,
    ) -> JoinOutput {
        Self::run_with_registry_and_env(nodes, edges, config, registry, &EnvBindingIndex::default())
    }

    /// Phase C (spec §6): same as [`run_with_registry`] but the
    /// caller passes the per-repo env-var bindings the
    /// `env_sensor` produced. The joiner resolves every
    /// `HostPart::Env([var])` against this index before falling
    /// through to the existing `services[].env` match — the
    /// env_sensor-resolved host is then matched against
    /// `services[].hosts`. Unmapped vars are recorded in
    /// [`JoinOutput::unresolved_env_vars`] (folded into the
    /// coverage ledger as `unresolved` records with
    /// `reason: EnvUnmapped`); conflicting multi-source hosts
    /// for the same var are recorded in
    /// [`JoinOutput::ambiguous_env_vars`] and the consumer is
    /// `Unresolved { reason: EnvAmbiguous }` (no `Binds` edge).
    pub fn run_with_registry_and_env(
        nodes: &[GraphNode],
        edges: &[GraphEdge],
        config: &ContractFederationConfig,
        registry: &ClientRegistry,
        env: &EnvBindingIndex,
    ) -> JoinOutput {
        // Step 1 — assign services to every node (§4.1). Longest
        // matching prefix wins; implicit service = repo id.
        let assignments = assign_services(nodes, config);

        // Step 2 — build endpoints (§7.2). Group provider nodes by
        // (service, method, template_after_prefixes). Code +
        // OpenAPI merge into one endpoint with two providers.
        let endpoint_table = build_endpoints(nodes, &assignments, config);

        // Step 3 — filter wrapper candidates (§7.3 rule 1).
        let http_clients = compile_http_clients(config);

        // Phase B (spec §5.3 tier 2): the cross-file client registry
        // the per-repo pre-pass built. The orchestrator passes it
        // through `run_with_registry`; `run` defaults to empty for
        // back-compat (every pre-Phase B test still passes an empty
        // registry, which preserves the §7.3 row order).
        let client_registry = registry;

        // Phase C (spec §6): the per-repo env-var bindings the
        // `env_sensor` produced. The joiner threads the index
        // through `resolve_consumer` so every
        // `HostPart::Env([var])` is checked against the
        // env_sensor before the existing `services[].env` match.
        let env_index = env;

        // Phase C accumulators — collected by `resolve_consumer`
        // and exposed on the `JoinOutput` for the orchestrator to
        // fold into the coverage ledger.
        let mut unresolved_env_vars: BTreeMap<String, u32> = BTreeMap::new();
        let mut ambiguous_env_vars: BTreeMap<String, Vec<String>> = BTreeMap::new();

        // Step 4 — resolve consumers (§7.3 table).
        let mut consumers: BTreeMap<GlobalId, ConsumerResolution> = BTreeMap::new();
        let mut external: BTreeMap<String, u32> = BTreeMap::new();
        let mut unnormalized: Vec<GlobalId> = Vec::new();
        let mut binds: Vec<BindsEdge> = Vec::new();
        // Protocol dispatch chain. Each protocol (Topic, RPC, GraphQL)
        // contributes one entry; the for-loop below iterates the chain
        // instead of the prior `if let Some(ContractFact::XxxConsumer)`
        // cascade. Adding a new protocol is one Vec entry plus the
        // matching `ProtocolDispatch` impl — no edits to this for-loop.
        let dispatch_chain: Vec<Box<dyn ProtocolDispatch>> = default_dispatch_chain();
        for node in nodes {
            let Some(fact) = node.contract.as_ref() else {
                continue;
            };
            let call_id = match GlobalId::parse(&node.id) {
                Ok(g) => g,
                Err(_) => continue,
            };
            let own_service = assignments
                .get(call_id.as_str())
                .cloned()
                .unwrap_or_else(|| implicit_service(node));
            // First match wins; the chain is ordered so the more
            // specific consumer shapes (Topic/Rpc/Graphql) are
            // tried before the HTTP fallback below.
            let mut dispatched = false;
            for dispatcher in &dispatch_chain {
                if dispatcher.matches(fact) {
                    let resolution = dispatcher.dispatch(
                        &call_id,
                        &own_service,
                        fact,
                        &endpoint_table,
                        config,
                        &mut binds,
                    );
                    consumers.insert(call_id.clone(), resolution);
                    dispatched = true;
                    break;
                }
            }
            if dispatched {
                continue;
            }
            let ContractFact::Consumer(consumer) = fact else {
                continue;
            };
            // Rule 1 — Phase A rule-1 fix. Wrapper candidates with
            // no matching `http_clients` entry are NOT silently
            // discarded (the pre-Phase-A bug). Instead, they are
            // recorded as `Unresolved { reason: WrapperUnconfigured }`
            // so the coverage ledger's `unresolved` bucket counts
            // them and `evaluate()` can downgrade `NoKnownImpact` on
            // a `CouldMatch` verdict (§9.5, §9.7). Rule 2
            // (confirmed bindings) is applied in step 6 below.
            //
            // When `http_clients` is empty (no operator config), the
            // pre-Phase-A `continue` is preserved UNLESS the per-repo
            // client registry (`client_registry`, Phase B §5.1) has
            // a `ClientDef` for this receiver — in that case the
            // joiner's tier-2 path (spec §5.3) has a known base and
            // the operator's expectation is that the receiver
            // resolves. The registry is the cross-file client
            // pre-pass output; the joiner consults it before
            // dropping.
            if is_wrapper_candidate(consumer) {
                let registry_has_def = matches!(
                    consumer.via,
                    CallVia::Receiver { ref expr, .. } if registry_lookup_name(client_registry, expr).is_some()
                );
                if http_clients.is_empty() && !registry_has_def {
                    continue;
                }
                if !http_clients.matches(&consumer.via) && !registry_has_def {
                    let own_service_for_unresolved = assignments
                        .get(call_id.as_str())
                        .cloned()
                        .unwrap_or_else(|| implicit_service(node));
                    consumers.insert(
                        call_id.clone(),
                        ConsumerResolution {
                            call_id: call_id.clone(),
                            service: own_service_for_unresolved,
                            target: Some(ConsumerTarget::Unresolved {
                                reason: UnresolvedReason::WrapperUnconfigured,
                                target_service: None,
                            }),
                            bound_endpoints: Vec::new(),
                            reads_complete: consumer.reads_complete,
                        },
                    );
                    continue;
                }
            }
            // The remaining rows (3, 4, 5, 6) are checked in
            // `resolve_consumer` below, in §7.3 table order. The
            // order matters: rule 3 (target service known)
            // beats rule 5 (template=None) — a dynamic-path
            // call whose host resolves to a known service
            // becomes rule 3's `no_route_in_service`, not
            // `unnormalized`.
            let own_service = assignments
                .get(call_id.as_str())
                .cloned()
                .unwrap_or_else(|| implicit_service(node));
            let resolution = resolve_consumer(
                &call_id,
                consumer,
                &own_service,
                config,
                &http_clients,
                &endpoint_table,
                &mut binds,
                &mut external,
                client_registry,
                env_index,
                &mut unresolved_env_vars,
                &mut ambiguous_env_vars,
            );
            // Rule 5 records the consumer in the
            // `unnormalized` index. The verdict lives on the
            // resolution itself; we mirror it into the
            // federation-wide `unnormalized` Vec here so the
            // surface tools can list candidates by id.
            if matches!(
                resolution.target,
                Some(ConsumerTarget::Unresolved {
                    reason: UnresolvedReason::Unnormalized,
                    ..
                })
            ) {
                unnormalized.push(call_id.clone());
            }
            consumers.insert(call_id.clone(), resolution);
        }

        // Step 6 — apply confirmed bindings (§7.6). Replaces the
        // rule 4 verdict for every matching call. Stale entries are
        // recorded.
        let mut stale_bindings = Vec::new();
        for (idx, binding) in config.bindings.iter().enumerate() {
            apply_confirmed_binding(
                binding,
                idx,
                nodes,
                &assignments,
                &endpoint_table,
                &mut binds,
                &mut consumers,
                &mut stale_bindings,
            );
        }

        // Step 5 — field join (§7.5, PR 9).
        //
        // Collect the response schemas per endpoint, then resolve
        // every `FieldRef` against the endpoints its call binds to.
        // The function lives in `field_join.rs`; this is the
        // integration point.
        let field_ref_nodes: Vec<&GraphNode> = nodes
            .iter()
            .filter(|n| {
                n.node_type == crate::schema::NodeType::FieldRef
                    && matches!(n.contract.as_ref(), Some(ContractFact::FieldRead(_)))
            })
            .collect();
        // `ContractJoiner::run` doesn't have `&[GraphNode]` lifetimes
        // for `&'a`, so re-borrow to owned references.
        let field_ref_nodes_owned: Vec<GraphNode> =
            field_ref_nodes.iter().map(|n| (*n).clone()).collect();
        let reads_from_edges: Vec<GraphEdge> = edges
            .iter()
            .filter(|e| e.edge_type == crate::schema::EdgeType::ReadsFrom)
            .cloned()
            .collect();
        let schemas: EndpointSchemas = collect_endpoint_schemas(nodes, edges, &assignments);
        let (field_refs, _schemaless, _unknown, field_binds) =
            resolve_field_refs(&field_ref_nodes_owned, &reads_from_edges, &binds, &schemas);
        // §7.5 step 2/3 emit `Binds(FieldRef → Field)` edges — merge
        // them into the desired bind set so step 7 sorts/dedups them
        // with the call binds and `rejoin_contracts` persists them
        // (§4.2 edge table; §9.5 traces `Field ← Binds ← FieldRef`).
        binds.extend(field_binds);
        // A FieldRef whose call has no bind yet (an
        // unresolved / external / unnormalized consumer) is recorded
        // as `unknown = true` by `resolve_field_refs` already. The
        // service on its `FieldRefResolution` falls back to
        // `"unknown"` in that branch.

        // Step 7 — sort every output collection by its key.
        binds.sort_by(|a, b| {
            a.consumer
                .as_str()
                .cmp(b.consumer.as_str())
                .then_with(|| a.provider.as_str().cmp(b.provider.as_str()))
        });
        // A confirmed binding can attach to a call that rule 3
        // also matched. De-dup by `(consumer, provider)`; the
        // Confirmed entry must win because §7.6 is the operator's
        // explicit override of the heuristic verdict.
        let mut deduped: Vec<BindsEdge> = Vec::with_capacity(binds.len());
        for edge in binds.into_iter() {
            if let Some(last) = deduped.last_mut() {
                if last.consumer == edge.consumer && last.provider == edge.provider {
                    if matches!(edge.provenance, EdgeProvenance::Confirmed { .. })
                        && !matches!(last.provenance, EdgeProvenance::Confirmed { .. })
                    {
                        *last = edge;
                    }
                    continue;
                }
            }
            deduped.push(edge);
        }
        binds = deduped;
        unnormalized.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        stale_bindings.sort_by(|a, b| {
            a.consumer
                .repo
                .as_str()
                .cmp(b.consumer.repo.as_str())
                .then_with(|| a.consumer.path.cmp(&b.consumer.path))
                .then_with(|| a.consumer.symbol.cmp(&b.consumer.symbol))
                .then_with(|| a.consumer.key.cmp(&b.consumer.key))
        });

        // Build the index.
        let mut services: BTreeMap<ServiceName, ServiceInfo> = BTreeMap::new();
        for s in &config.services {
            let name = ServiceName(s.name.clone());
            let mut endpoint_ids: Vec<EndpointId> = Vec::new();
            for (svc, key) in endpoint_table.keys() {
                if svc == &name {
                    endpoint_ids.push((svc.clone(), key.clone()));
                }
            }
            endpoint_ids.sort_by_key(endpoint_id_string);
            services.insert(
                name.clone(),
                ServiceInfo {
                    name,
                    repo: RepoId::new(&s.repo).unwrap_or_else(|_| RepoId::new("unknown").unwrap()),
                    paths: s.paths.clone(),
                    hosts: s.hosts.clone(),
                    env: s.env.clone(),
                    base_path: s.base_path.clone(),
                    route_prefixes: s
                        .route_prefixes
                        .iter()
                        .map(|r| CfgRoutePrefix {
                            path: r.path.clone(),
                            prefix: r.prefix.clone(),
                        })
                        .collect(),
                    endpoint_ids,
                },
            );
        }
        let mut endpoints: BTreeMap<EndpointId, Endpoint> = BTreeMap::new();
        for ((service, key), providers) in endpoint_table {
            let method = match &key {
                ContractKey::Http { method, .. } => match method {
                    MethodSpec::Known(m) => *m,
                    MethodSpec::Unknown => HttpMethod::Any,
                },
                ContractKey::Topic { .. } => HttpMethod::Any,
                ContractKey::Rpc { .. } => HttpMethod::Any,
                ContractKey::Graphql { .. } => HttpMethod::Any,
            };
            let template = key.leaf().to_string();
            let provider_records: Vec<EndpointProvider> = providers
                .into_iter()
                .map(|p| {
                    let origin = p
                        .fact
                        .as_ref()
                        .and_then(|f| {
                            if let ContractFact::Provider(pf) = f {
                                Some(pf.origin)
                            } else {
                                None
                            }
                        })
                        .unwrap_or(ProviderOrigin::Code);
                    let (handler, operation_id) = p
                        .fact
                        .as_ref()
                        .and_then(|f| {
                            if let ContractFact::Provider(pf) = f {
                                Some((pf.handler.clone(), pf.operation_id.clone()))
                            } else {
                                None
                            }
                        })
                        .unwrap_or((None, None));
                    EndpointProvider {
                        node_id: p.id,
                        origin,
                        handler,
                        operation_id,
                    }
                })
                .collect();
            // Collect the per-direction schemas for this endpoint
            // (§7.5 feeds off `Response`; `Request` is here for the
            // request-side diff in PR 12+). The schema node id is
            // derived from the route's first provider so a reader can
            // jump back to the source.
            let endpoint_id = (service.clone(), key.clone());
            let mut endpoint_schemas: BTreeMap<
                Direction,
                crate::federation::contracts::index::EndpointSchema,
            > = BTreeMap::new();
            if let Some(by_dir) = schemas.by_endpoint.get(&endpoint_id) {
                for (dir, fields) in by_dir {
                    // The schema node id is the openapi `Schema` node
                    // for (endpoint, direction) — `HasField` edges
                    // hang off it, so tools can walk `Schema →
                    // Field`. Fall back to the first provider only
                    // for endpoints with no schema node (code-only
                    // routes are schemaless).
                    let schema_node_id = schemas
                        .schema_node
                        .get(&endpoint_id)
                        .and_then(|m| m.get(dir))
                        .cloned()
                        .unwrap_or_else(|| {
                            provider_records
                                .first()
                                .map(|p| p.node_id.clone())
                                .unwrap_or_else(|| GlobalId::from_string("unknown"))
                        });
                    let mut field_map: BTreeMap<
                        crate::federation::contracts::model::JsonPath,
                        crate::federation::contracts::model::FieldMeta,
                    > = BTreeMap::new();
                    for f in fields {
                        field_map.insert(f.path.clone(), f.meta.clone());
                    }
                    endpoint_schemas.insert(
                        *dir,
                        crate::federation::contracts::index::EndpointSchema {
                            node_id: schema_node_id,
                            fields: field_map,
                        },
                    );
                }
            }
            endpoints.insert(
                endpoint_id,
                Endpoint {
                    id: (service, key),
                    method,
                    template,
                    providers: provider_records,
                    schemas: endpoint_schemas,
                },
            );
        }

        let index = ContractIndex {
            services,
            endpoints,
            consumers,
            field_refs,
            stale_bindings,
            external,
            unnormalized,
        };
        JoinOutput {
            binds,
            index,
            unresolved_env_vars,
            ambiguous_env_vars,
        }
    }
}

// ─── helpers ───────────────────────────────────────────────────────────

/// Compact human-readable sort key for an `EndpointId`. Used to keep
/// `ServiceInfo.endpoint_ids` sorted by the same lexicographic order
/// the §4.4 grammar implies.
fn endpoint_id_string(id: &EndpointId) -> String {
    id.0.to_string() + "|" + &id.1.to_string()
}

/// `(service, ContractKey)` from a normalized provider template +
/// service-level `base_path` / `route_prefixes`. Empty `base_path` is
/// a no-op; `route_prefixes` only prepends when the provider's
/// `path` matches the prefix entry's `path`.
fn endpoint_template_for(
    provider: &ProviderFact,
    node_path: &str,
    service: &ServiceDecl,
) -> String {
    let mut t = String::new();
    if let Some(bp) = &service.base_path {
        t.push_str(bp);
    }
    for rp in &service.route_prefixes {
        if rp.path == node_path {
            t.push_str(&rp.prefix);
        }
    }
    t.push_str(&provider.template);
    t
}

#[derive(Debug, Clone)]
pub struct EndpointProviderRecord {
    pub id: GlobalId,
    pub fact: Option<ContractFact>,
    pub template: String,
    pub method: HttpMethod,
}

/// Step 1: assign a service to every node. The longest matching
/// `paths` prefix wins; ties go to the first service in declaration
/// order. Nodes whose `repo_id` is `None` (single-workspace graphs)
/// use the implicit service name = repo id from the node's
/// `GlobalId::repo_id`.
fn assign_services(
    nodes: &[GraphNode],
    config: &ContractFederationConfig,
) -> BTreeMap<String, ServiceName> {
    let mut out: BTreeMap<String, ServiceName> = BTreeMap::new();
    for node in nodes {
        let repo_id_str = match node.repo_id.as_deref() {
            Some(s) => s.to_string(),
            None => match GlobalId::parse(&node.id) {
                Ok(g) => g.repo_id().to_string(),
                Err(_) => continue,
            },
        };
        let mut best: Option<(&ServiceDecl, usize)> = None;
        for s in &config.services {
            if s.repo != repo_id_str {
                continue;
            }
            let matched_prefix: Option<usize> = if s.paths.is_empty() {
                Some(0)
            } else {
                s.paths
                    .iter()
                    .filter(|p| node.path.starts_with(p.as_str()))
                    .map(|p| p.len())
                    .max()
            };
            if let Some(prefix) = matched_prefix {
                match best {
                    Some((_, best_len)) if best_len >= prefix => {}
                    _ => best = Some((s, prefix)),
                }
            }
        }
        let svc = match best {
            Some((s, _)) => ServiceName(s.name.clone()),
            None => ServiceName(repo_id_str.clone()),
        };
        out.insert(node.id.clone(), svc);
    }
    out
}

/// Implicit service name for a node that the config does not
/// explicitly cover. Matches §4.1: the implicit service is named
/// after the repo id.
fn implicit_service(node: &GraphNode) -> ServiceName {
    let repo = node
        .repo_id
        .clone()
        .or_else(|| {
            GlobalId::parse(&node.id)
                .ok()
                .map(|g| g.repo_id().to_string())
        })
        .unwrap_or_else(|| "unknown".into());
    ServiceName(repo)
}

/// Step 2: group provider nodes into endpoints. A code route and an
/// OpenAPI operation with the same `(service, method, template)`
/// merge into one endpoint with two providers. Topic providers (§6.7)
/// are keyed by `(service, ContractKey::Topic { broker, name })` so
/// the topic-join path can match them later. RPC providers (§8.2)
/// are keyed by `(service, ContractKey::Rpc { system, service,
/// method })` so the rpc-join path can match them later.
fn build_endpoints(
    nodes: &[GraphNode],
    assignments: &BTreeMap<String, ServiceName>,
    config: &ContractFederationConfig,
) -> BTreeMap<(ServiceName, ContractKey), Vec<EndpointProviderRecord>> {
    let mut table: BTreeMap<(ServiceName, ContractKey), Vec<EndpointProviderRecord>> =
        BTreeMap::new();
    for node in nodes {
        let Ok(gid) = GlobalId::parse(&node.id) else {
            continue;
        };
        let svc = match assignments.get(node.id.as_str()) {
            Some(s) => s,
            None => continue,
        };
        match node.contract.as_ref() {
            Some(ContractFact::Provider(provider)) => {
                // Resolve the service decl (or fall back to a zeroed one for
                // implicit services that the config did not declare).
                let service_decl: ServiceDecl = config
                    .services
                    .iter()
                    .find(|s| s.name == svc.0)
                    .cloned()
                    .unwrap_or_else(|| ServiceDecl {
                        name: svc.0.clone(),
                        repo: svc.0.clone(),
                        paths: Vec::new(),
                        hosts: Vec::new(),
                        env: Vec::new(),
                        base_path: None,
                        route_prefixes: Vec::new(),
                    });
                let full_template = endpoint_template_for(provider, &node.path, &service_decl);
                let method = match &provider.method {
                    HttpMethod::Any => HttpMethod::Any,
                    other => *other,
                };
                // §6.7: producers on `Topic` nodes use the
                // `ContractKey::Topic { broker, name }` form so the
                // topic-join path can match them by `(broker, name)`.
                // The template carries the topic name verbatim;
                // `provider.template` is what the event sensor wrote
                // (`topic_name`) and it is never prefixed — topics
                // don't participate in `route_prefixes` / `base_path`.
                if matches!(node.node_type, crate::schema::NodeType::Topic) {
                    let broker = default_broker_for(node);
                    let key = ContractKey::Topic {
                        broker,
                        name: full_template.clone(),
                    };
                    table
                        .entry((svc.clone(), key))
                        .or_default()
                        .push(EndpointProviderRecord {
                            id: gid,
                            fact: Some(ContractFact::Provider(provider.clone())),
                            template: full_template,
                            method,
                        });
                    continue;
                }
                let key = ContractKey::Http {
                    method: MethodSpec::Known(method),
                    template: full_template.clone(),
                };
                table
                    .entry((svc.clone(), key))
                    .or_default()
                    .push(EndpointProviderRecord {
                        id: gid,
                        fact: Some(ContractFact::Provider(provider.clone())),
                        template: full_template,
                        method,
                    });
            }
            // Phase E (spec §8.2): an `RpcProvider` node carries the
            // exact `(system, service, method)` triple the
            // `ContractKey::Rpc` form needs. The endpoint table
            // indexes the provider by that key so the
            // `resolve_rpc_consumer` join (Task 5) can do an
            // exact-match lookup without the URL-prefix tolerance
            // the HTTP path uses.
            Some(ContractFact::RpcProvider(rpc)) => {
                let key = ContractKey::Rpc {
                    system: rpc.system,
                    service: rpc.service.clone(),
                    method: rpc.method.clone(),
                };
                table
                    .entry((svc.clone(), key))
                    .or_default()
                    .push(EndpointProviderRecord {
                        id: gid,
                        fact: Some(ContractFact::RpcProvider(rpc.clone())),
                        template: rpc.method.clone(),
                        method: HttpMethod::Any,
                    });
            }
            // Phase E (spec §8.3): a `GraphqlProvider` node
            // carries the `(op, field)` pair the
            // `ContractKey::Graphql` form needs. The endpoint
            // table indexes the provider by that key so the
            // `resolve_graphql_consumer` join (Task 5) can do
            // an exact-match lookup within the service that
            // owns the `/graphql` HTTP route.
            Some(ContractFact::GraphqlProvider(graphql_provider)) => {
                let key = ContractKey::Graphql {
                    op: graphql_provider.op,
                    field: graphql_provider.field.clone(),
                };
                table
                    .entry((svc.clone(), key))
                    .or_default()
                    .push(EndpointProviderRecord {
                        id: gid,
                        fact: Some(ContractFact::GraphqlProvider(graphql_provider.clone())),
                        template: graphql_provider.field.clone(),
                        method: HttpMethod::Any,
                    });
            }
            _ => continue,
        }
    }
    table
}

/// §7.7: a topic without a declared broker is `kafka`.
fn default_broker_for(node: &GraphNode) -> String {
    let name = &node.name;
    if let Some((broker, _)) = name.split_once('/') {
        if !broker.is_empty() {
            return broker.to_string();
        }
    }
    "kafka".to_string()
}

fn is_wrapper_candidate(consumer: &crate::federation::contracts::model::ConsumerFact) -> bool {
    matches!(consumer.via, CallVia::Receiver { .. })
}

/// Phase B (spec §5.1 + §5.3 tier 2): does the registry carry any
/// `ClientDef` whose `name == query_name`? A hit means the joiner
/// can compose `base ++ call_path` for this receiver — the
/// rule-1 gate must NOT drop the call. Cross-file resolution lives
/// on `ClientDef::resolve_cross_file`; this helper is the bare-name
/// check the gate needs.
fn registry_lookup_name<'a>(
    registry: &'a ClientRegistry,
    query_name: &str,
) -> Option<&'a ClientDef> {
    registry
        .iter()
        .find(|(_, name, _)| *name == query_name)
        .map(|(_, _, def)| def)
}

#[derive(Debug, Clone, Default)]
pub struct CompiledHttpClients {
    entries: Vec<CompiledHttpClient>,
}

impl CompiledHttpClients {
    /// Public constructor for tests / property-test harnesses.
    /// Builds a `CompiledHttpClients` from a slice of
    /// [`HttpClientDecl`] without going through the full
    /// `ContractFederationConfig`. Production callers use
    /// [`compile_http_clients`] instead.
    ///
    /// This is the only path that builds a `CompiledHttpClients`
    /// without a `ContractFederationConfig`; the property tests in
    /// `tests/property_join_pipeline.rs` exercise the joiner's I2 /
    /// I5 / I6 total-order invariants without an orchestrator.
    pub fn compile_for_tests(
        decls: &[crate::federation::contracts::config::HttpClientDecl],
    ) -> Self {
        let entries = decls
            .iter()
            .map(|e| CompiledHttpClient {
                pattern: e.call.clone(),
                service: ServiceName(e.service.clone()),
                method: e.method.as_ref().and_then(|m| parse_method(m)),
                path_arg: e.path_arg,
            })
            .collect();
        Self { entries }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // method / path_arg are reserved for future filtering (§7.3 rule 3).
struct CompiledHttpClient {
    pattern: String,
    service: ServiceName,
    method: Option<HttpMethod>,
    path_arg: Option<u8>,
}

impl CompiledHttpClients {
    fn matches(&self, via: &CallVia) -> bool {
        let CallVia::Receiver { expr, fn_name, .. } = via else {
            return false;
        };
        // The §7.3 / §7.1 grammar is "the call's `expr.fn_name`
        // string" — the receiver joined to the method by `.`.
        // `expr.fn_name` here is "ordersClient.get" when the call
        // is `ordersClient.get(...)` and the config pattern is
        // `ordersClient.{method}`.
        let combined = format!("{expr}.{fn_name}");
        self.entries
            .iter()
            .any(|e| pattern_matches(&e.pattern, &combined))
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn pattern_matches(pattern: &str, fn_name: &str) -> bool {
    // `pattern` may contain `{method}`; we don't constrain it at this
    // stage — the joiner records the binding either way (the
    // service-level resolution checks the consumer's method below).
    if let Some(idx) = pattern.find("{method}") {
        let head = &pattern[..idx];
        let tail = &pattern[idx + "{method}".len()..];
        fn_name.starts_with(head)
            && fn_name.ends_with(tail)
            && fn_name.len() >= head.len() + tail.len()
    } else {
        fn_name == pattern
    }
}

fn compile_http_clients(config: &ContractFederationConfig) -> CompiledHttpClients {
    let entries = config
        .http_clients
        .iter()
        .map(|e| CompiledHttpClient {
            pattern: e.call.clone(),
            service: ServiceName(e.service.clone()),
            method: e.method.as_ref().and_then(|m| parse_method(m)),
            path_arg: e.path_arg,
        })
        .collect();
    CompiledHttpClients { entries }
}

fn parse_method(s: &str) -> Option<HttpMethod> {
    Some(match s {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        "PUT" => HttpMethod::Put,
        "PATCH" => HttpMethod::Patch,
        "DELETE" => HttpMethod::Delete,
        "HEAD" => HttpMethod::Head,
        "OPTIONS" => HttpMethod::Options,
        "ANY" => HttpMethod::Any,
        _ => return None,
    })
}

fn host_is_external_exempt(host: &str) -> bool {
    matches!(
        host,
        "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]" | "host.docker.internal"
    )
}

/// §7.7 (stretch, PR 15): resolve a topic consumer. Same broker AND
/// same name = `Binds { route_match: Exact, confidence: 1.0 }`. Any
/// mismatch (different broker, different name, no producer-side
/// endpoint) = `Unresolved { reason: NoMatch }`. The HTTP §7.3
/// table doesn't apply — topics don't have hosts / templates, only
/// the `(broker, name)` pair. Mirrors `resolve_consumer` in spirit
/// but skips host / env / http_clients dispatch.
///
/// §7.8 invariant: every `Binds` edge connects two different
/// services. An in-service topic consumer must NOT bind to its own
/// service's producer endpoint — same rule §7.3 rule 6 enforces
/// for HTTP consumers.
pub(crate) fn resolve_topic_consumer(
    call_id: &GlobalId,
    consumer: &TopicConsumerFact,
    own_service: &ServiceName,
    endpoints: &BTreeMap<(ServiceName, ContractKey), Vec<EndpointProviderRecord>>,
    binds: &mut Vec<BindsEdge>,
) -> ConsumerResolution {
    let key = ContractKey::Topic {
        broker: consumer.broker.clone(),
        name: consumer.name.clone(),
    };
    // Find every producer-side endpoint with the same `(broker, name)`.
    let mut bound: Vec<EndpointId> = Vec::new();
    for ((svc, k), providers) in endpoints {
        if k != &key {
            continue;
        }
        // §7.8: in-service calls are not a contract between
        // services. Skip own-service producers.
        if svc == own_service {
            continue;
        }
        if let Some(provider) = providers.first() {
            binds.push(BindsEdge {
                consumer: call_id.clone(),
                provider: provider.id.clone(),
                consumer_service: own_service.clone(),
                provider_service: svc.clone(),
                target_endpoint: (svc.clone(), k.clone()),
                provenance: EdgeProvenance::Static {
                    source: crate::schema::StaticSource::Regex,
                },
                confidence: 1.0,
                route_match: RouteMatch::Exact,
                stripped_prefix: None,
            });
            bound.push((svc.clone(), k.clone()));
        }
    }
    bound.sort_by(|a, b| {
        a.0 .0
            .cmp(&b.0 .0)
            .then_with(|| a.1.to_string().cmp(&b.1.to_string()))
    });
    let target = if bound.is_empty() {
        Some(ConsumerTarget::Unresolved {
            reason: UnresolvedReason::NoMatch,
            target_service: None,
        })
    } else {
        Some(ConsumerTarget::Binds {
            provenance: EdgeProvenance::Static {
                source: crate::schema::StaticSource::Regex,
            },
            confidence: 1.0,
            route_match: RouteMatch::Exact,
            stripped_prefix: None,
        })
    };
    ConsumerResolution {
        call_id: call_id.clone(),
        service: own_service.clone(),
        target,
        bound_endpoints: bound,
        reads_complete: true,
    }
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
///
/// Returns a `ConsumerResolution` mirroring the RPC / topic
/// paths and pushes any `Binds` edges onto `binds`.
pub(crate) fn resolve_graphql_consumer(
    call_id: &GlobalId,
    consumer: &crate::federation::contracts::model::GraphqlConsumerFact,
    own_service: &ServiceName,
    endpoints: &BTreeMap<(ServiceName, ContractKey), Vec<EndpointProviderRecord>>,
    binds: &mut Vec<BindsEdge>,
) -> ConsumerResolution {
    // Determine the `/graphql` route owner for the
    // `target_service` field on the unresolved record. This
    // is purely informational — the candidate set is keyed
    // on `(op, field)` and may include providers in
    // services other than the route owner.
    let mut graphql_route_owners: Vec<ServiceName> = Vec::new();
    for (svc, key) in endpoints.keys() {
        if let ContractKey::Http { method, template } = key {
            if template == "/graphql"
                && matches!(
                    method,
                    MethodSpec::Known(HttpMethod::Post) | MethodSpec::Unknown
                )
            {
                graphql_route_owners.push(svc.clone());
            }
        }
    }
    graphql_route_owners.sort();
    graphql_route_owners.dedup();
    let graphql_route_owner: Option<ServiceName> = match graphql_route_owners.len() {
        0 => None,
        1 => Some(graphql_route_owners[0].clone()),
        _ => None,
    };
    let target_key = ContractKey::Graphql {
        op: consumer.op,
        field: consumer.field.clone(),
    };
    let mut candidates: Vec<&EndpointProviderRecord> = Vec::new();
    for ((svc, key), providers) in endpoints {
        if key != &target_key {
            continue;
        }
        // I5: skip own-service providers (same-service binds
        // are not a contract between services).
        if svc == own_service {
            continue;
        }
        for provider in providers {
            candidates.push(provider);
        }
    }
    match candidates.len() {
        0 => ConsumerResolution {
            call_id: call_id.clone(),
            service: own_service.clone(),
            target: Some(ConsumerTarget::Unresolved {
                reason: UnresolvedReason::GraphqlNoOp,
                target_service: graphql_route_owner.clone(),
            }),
            bound_endpoints: Vec::new(),
            reads_complete: true,
        },
        1 => {
            let provider = candidates[0];
            let provenance = EdgeProvenance::Static {
                source: crate::schema::StaticSource::Regex,
            };
            let endpoint_id = (provider_service_of(provider, endpoints), target_key.clone());
            binds.push(BindsEdge {
                consumer: call_id.clone(),
                provider: provider.id.clone(),
                consumer_service: own_service.clone(),
                provider_service: endpoint_id.0.clone(),
                target_endpoint: endpoint_id.clone(),
                provenance,
                confidence: 1.0,
                route_match: RouteMatch::Exact,
                stripped_prefix: None,
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
                bound_endpoints: vec![endpoint_id],
                reads_complete: true,
            }
        }
        _ => ConsumerResolution {
            call_id: call_id.clone(),
            service: own_service.clone(),
            target: Some(ConsumerTarget::Unresolved {
                reason: UnresolvedReason::GraphqlNoOp,
                target_service: graphql_route_owner.clone(),
            }),
            bound_endpoints: Vec::new(),
            reads_complete: true,
        },
    }
}

/// Resolve the `ServiceName` for an endpoint provider record
/// from the endpoint-table key. Each `EndpointProviderRecord`
/// is a value in a `BTreeMap<(ServiceName, ContractKey),
/// Vec<EndpointProviderRecord>>`; the key's service is the
/// canonical source. We plumb the map in (rather than
/// reaching for an `&HashMap` field on the record) so the
/// function is pure and the joiner stays a pure function of
/// its inputs (§7.8).
fn provider_service_of(
    provider: &EndpointProviderRecord,
    endpoints: &BTreeMap<(ServiceName, ContractKey), Vec<EndpointProviderRecord>>,
) -> ServiceName {
    for (svc, key) in endpoints.keys() {
        if let Some(records) = endpoints.get(&(svc.clone(), key.clone())) {
            if records.iter().any(|r| r.id == provider.id) {
                return svc.clone();
            }
        }
    }
    ServiceName("unknown".to_string())
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
///
/// Returns a `ConsumerResolution` (mirroring `resolve_topic_consumer`)
/// and pushes any `Binds` edges onto `binds`.
pub(crate) fn resolve_rpc_consumer(
    call_id: &GlobalId,
    consumer: &RpcConsumerFact,
    own_service: &ServiceName,
    endpoints: &BTreeMap<(ServiceName, ContractKey), Vec<EndpointProviderRecord>>,
    config: &ContractFederationConfig,
    binds: &mut Vec<BindsEdge>,
) -> ConsumerResolution {
    // Resolve the channel host to the set of candidate services.
    // The empty / missing case (HostPart::None) means the channel
    // could not be resolved — the stub call is unknown to the
    // joiner and lands in `Unresolved { reason: RpcStubUnknown }`
    // without iterating providers.
    let candidate_services: Vec<ServiceName> = match &consumer.channel_host_part {
        HostPart::None => {
            return ConsumerResolution {
                call_id: call_id.clone(),
                service: own_service.clone(),
                target: Some(ConsumerTarget::Unresolved {
                    reason: UnresolvedReason::RpcStubUnknown,
                    target_service: None,
                }),
                bound_endpoints: Vec::new(),
                reads_complete: true,
            };
        }
        HostPart::Literal(host) => {
            let mut svcs: Vec<ServiceName> = config
                .services
                .iter()
                .filter(|s| s.hosts.iter().any(|pat| host_matches_pattern(pat, host)))
                .map(|s| ServiceName(s.name.clone()))
                .collect();
            if svcs.is_empty() {
                return ConsumerResolution {
                    call_id: call_id.clone(),
                    service: own_service.clone(),
                    target: Some(ConsumerTarget::Unresolved {
                        reason: UnresolvedReason::RpcStubUnknown,
                        target_service: None,
                    }),
                    bound_endpoints: Vec::new(),
                    reads_complete: true,
                };
            }
            svcs.sort();
            svcs
        }
        HostPart::Env(_) | HostPart::Expr(_) => {
            // Env / Expr host: the spec defers to Phase C env
            // resolution (already plumbed through `target_service_from_hosts`).
            // We pass the literal resolution for now (the joiner
            // does not need Phase C to satisfy E1-E5).
            return ConsumerResolution {
                call_id: call_id.clone(),
                service: own_service.clone(),
                target: Some(ConsumerTarget::Unresolved {
                    reason: UnresolvedReason::RpcStubUnknown,
                    target_service: None,
                }),
                bound_endpoints: Vec::new(),
                reads_complete: true,
            };
        }
    };
    // The consumer's `service` is the bare service name
    // (`Orders`); the `package` field is empty at scan time
    // because the consumer sensor doesn't read the proto file.
    // The joiner fills the package by finding a provider whose
    // service name ends in `.<bare_service>` (e.g. `com.acme.orders.Orders`).
    let target_key = ContractKey::Rpc {
        system: consumer.system,
        service: consumer.service.clone(),
        method: consumer.method.clone(),
    };
    let mut bound: Vec<EndpointId> = Vec::new();
    // First pass: exact match on the consumer's bare service
    // identity, restricted to the candidate services. This
    // handles the case where the consumer's service name is
    // already package-qualified (e.g. a Java stub typed
    // `com.acme.orders.OrdersBlockingStub`).
    for ((svc, key), providers) in endpoints {
        if !candidate_services.contains(svc) {
            continue;
        }
        if key != &target_key {
            continue;
        }
        // §7.8 / I5: skip own-service providers — same-service
        // binds are not a contract between services.
        if svc == own_service {
            continue;
        }
        if let Some(provider) = providers.first() {
            let provenance = EdgeProvenance::Static {
                source: crate::schema::StaticSource::Regex,
            };
            binds.push(BindsEdge {
                consumer: call_id.clone(),
                provider: provider.id.clone(),
                consumer_service: own_service.clone(),
                provider_service: svc.clone(),
                target_endpoint: (svc.clone(), key.clone()),
                provenance,
                confidence: 1.0,
                route_match: RouteMatch::Exact,
                stripped_prefix: None,
            });
            bound.push((svc.clone(), key.clone()));
        }
    }
    // Second pass: package-qualified match. The consumer's
    // `service` is bare; we look for any provider whose
    // service name ends in `.<bare_service>`. The match is
    // exact on the method and the bare service name.
    if bound.is_empty() {
        let bare = format!(".{}", consumer.service);
        for ((svc, key), providers) in endpoints {
            if !candidate_services.contains(svc) {
                continue;
            }
            let ContractKey::Rpc { service, .. } = key else {
                continue;
            };
            if !service.ends_with(&bare) {
                continue;
            }
            let provider_method = match key {
                ContractKey::Rpc { method, .. } => method,
                _ => continue,
            };
            if provider_method != &consumer.method {
                continue;
            }
            if svc == own_service {
                continue;
            }
            if let Some(provider) = providers.first() {
                let provenance = EdgeProvenance::Static {
                    source: crate::schema::StaticSource::Regex,
                };
                binds.push(BindsEdge {
                    consumer: call_id.clone(),
                    provider: provider.id.clone(),
                    consumer_service: own_service.clone(),
                    provider_service: svc.clone(),
                    target_endpoint: (svc.clone(), key.clone()),
                    provenance,
                    confidence: 1.0,
                    route_match: RouteMatch::Exact,
                    stripped_prefix: None,
                });
                bound.push((svc.clone(), key.clone()));
            }
        }
    }
    let target = if bound.is_empty() {
        Some(ConsumerTarget::Unresolved {
            reason: UnresolvedReason::RpcStubUnknown,
            target_service: None,
        })
    } else {
        Some(ConsumerTarget::Binds {
            provenance: EdgeProvenance::Static {
                source: crate::schema::StaticSource::Regex,
            },
            confidence: 1.0,
            route_match: RouteMatch::Exact,
            stripped_prefix: None,
        })
    };
    ConsumerResolution {
        call_id: call_id.clone(),
        service: own_service.clone(),
        target,
        bound_endpoints: bound,
        reads_complete: true,
    }
}

/// Resolve one consumer. Returns a `ConsumerResolution` and appends
/// any `Binds` edges it produced. The 6-row §7.3 table runs in
/// order; the first match wins.
///
/// Spec §5.3 / I6 total-order precedence. The 6 tiers run in this
/// order; once a tier binds, no lower tier can override:
///
/// 1. Confirmed binding — handled separately by
///    `apply_confirmed_binding` (§7.6) before this function is
///    called.
/// 2. Code-derived base + host/env (Phase B [`ClientRegistry`] —
///    composes `base ++ call_path` via [`compose_and_normalize`]).
/// 3. `http_clients` config pattern (rule 3 first branch in §7.3).
/// 4. operationId match (Heuristic 0.9, PR 18 — runs inside
///    `match_one_service` after a URL match attempt fails).
/// 5. Unbound-host heuristic (0.6) / ambiguous (0.3) (rule 6 in
///    §7.3).
/// 6. Unresolved candidate (rule 5 in §7.3 when template is
///    `None`; otherwise the reason recorded on the verdict).
///
/// `client_registry` is the Phase B wrapper-base registry. An
/// empty registry skips tier 2 entirely (every existing pre-Phase B
/// test passes an empty registry, preserving the §7.3 row order).
///
/// Public alias: [`resolve_consumer_to_service`] (same call shape + same I6
/// total order, exposed at the joiner module's surface so the property tests
/// in `tests/property/join_pipeline.rs` can pin I2 / I5 / I6 without
/// round-tripping through the full `ContractJoiner::run` orchestrator).
#[allow(clippy::too_many_arguments)]
pub fn resolve_consumer_to_service(
    consumer: &ConsumerFact,
    own_service: &ServiceName,
    config: &ContractFederationConfig,
    endpoints: &BTreeMap<(ServiceName, ContractKey), Vec<EndpointProviderRecord>>,
    http_clients: &CompiledHttpClients,
    external: &mut BTreeMap<String, u32>,
    binds: &mut Vec<BindsEdge>,
    client_registry: &ClientRegistry,
) -> ConsumerResolution {
    // The orchestrator's `GlobalId` isn't observable from a
    // single-call surface — the property tests construct synthetic
    // ids. We mint a deterministic one from the consumer's
    // `url_expr` (already canonicalized by the sensor) so the
    // resolution surfaces the same `call_id` on every run (I4).
    let synthetic = GlobalId::from_string(&format!(
        "synthetic:HttpClientCall:synthetic:{}",
        consumer.url_expr
    ));
    let mut unresolved_env = BTreeMap::new();
    let mut ambiguous_env: BTreeMap<String, Vec<String>> = BTreeMap::new();
    resolve_consumer(
        &synthetic,
        consumer,
        own_service,
        config,
        http_clients,
        endpoints,
        binds,
        external,
        client_registry,
        &EnvBindingIndex::default(),
        &mut unresolved_env,
        &mut ambiguous_env,
    )
}
///
/// `client_registry` is the Phase B wrapper-base registry. An
/// empty registry skips tier 2 entirely (every existing pre-Phase B
/// test passes an empty registry, preserving the §7.3 row order).
///
/// `env` is the Phase C env_sensor's per-repo index; the
/// `unresolved_env_vars` / `ambiguous_env_vars` accumulators are
/// filled in when a `HostPart::Env([var])` consumer has no binding
/// or binds to conflicting hosts.
#[allow(clippy::too_many_arguments)]
fn resolve_consumer(
    call_id: &GlobalId,
    consumer: &crate::federation::contracts::model::ConsumerFact,
    own_service: &ServiceName,
    config: &ContractFederationConfig,
    http_clients: &CompiledHttpClients,
    endpoints: &BTreeMap<(ServiceName, ContractKey), Vec<EndpointProviderRecord>>,
    binds: &mut Vec<BindsEdge>,
    external: &mut BTreeMap<String, u32>,
    client_registry: &ClientRegistry,
    env: &EnvBindingIndex,
    unresolved_env_vars: &mut BTreeMap<String, u32>,
    ambiguous_env_vars: &mut BTreeMap<String, Vec<String>>,
) -> ConsumerResolution {
    let target_template = consumer.url.template.clone();
    let target_method = consumer.method.clone();
    // Phase C (spec §6): when the env_sensor reports a var as
    // unmapped AND no other rule resolves the consumer, the
    // final verdict is `Unresolved { reason: EnvUnmapped }` —
    // not the legacy `NoMatch`. The env_sensor's Unmapped is
    // authoritative for `HostPart::Env` consumers (spec §6
    // acceptance: "with no mapping, appears in `env_unmapped`").
    // The flag is set when the env_sensor path returns
    // `Unmapped`; lower tiers may still resolve the call (env-
    // name match, host match, rule 4, rule 6), so the flag is
    // only consulted at the very end when no tier bound.
    let mut env_unmapped_pending = false;

    // Tier 2 — code-derived base + host/env (Phase B). When the
    // call's `via` resolves to a `ClientDef` in the registry,
    // compose the base with the call's URL parts and use the
    // composed target's host as the resolution axis. Tier 2 beats
    // tiers 3–6 because the operator has declared the wrapper's
    // base explicitly (the spec §5.3 acceptance: "with a known
    // base → binds to the Orders service with provenance naming
    // the client").
    if let Some(svc) = target_service_via_registry(consumer, client_registry, config) {
        let resolution = match_one_service(
            call_id,
            consumer,
            own_service,
            svc,
            &target_method,
            target_template.as_deref(),
            true,
            endpoints,
            binds,
        );
        return resolution;
    }

    // Tier 3 — http_clients config pattern (rule 3 first branch in
    // §7.3). Fires before env/hosts because a wrapper pattern is
    // more specific than a host or env-name match.
    if let Some(svc) = target_service_from_http_client(consumer, http_clients) {
        let resolution = match_one_service(
            call_id,
            consumer,
            own_service,
            svc,
            &target_method,
            target_template.as_deref(),
            true,
            endpoints,
            binds,
        );
        return resolution;
    }
    // Phase C (spec §6): `HostPart::Env([var])` → env_sensor → host
    // → `services[].hosts`. Resolves the env var via the sensor's
    // bindings and matches the resulting host against a service's
    // `hosts` list. Three outcomes:
    //
    // - All vars resolve to the same host AND the host matches
    //   a service's `hosts` list → `match_one_service` returns
    //   the resolution.
    // - Vars resolve to different hosts → `EnvAmbiguous`, no
    //   bind, the consumer is recorded as
    //   `Unresolved { reason: EnvAmbiguous }`. Per I6 the
    //   ambiguous case preempts every other tier — once vars
    //   disagree, no lower tier can override.
    // - Any var has no binding → the var counter is bumped
    //   in `unresolved_env_vars` for the ledger, but the
    //   joiner falls through to the existing `target_service_from_env`
    //   (`services[].env` name match). Spec §6: "in addition
    //   to the existing `services[].env` match" — the new
    //   path is additive, not a replacement.
    if let HostPart::Env(names) = &consumer.url.host {
        if let Some(resolved) =
            resolve_env_consumer(consumer, env, config, unresolved_env_vars)
        {
            match resolved {
                EnvResolution::Service(svc) => {
                    let resolution = match_one_service(
                        call_id,
                        consumer,
                        own_service,
                        svc,
                        &target_method,
                        target_template.as_deref(),
                        true,
                        endpoints,
                        binds,
                    );
                    return resolution;
                }
                EnvResolution::Ambiguous(hosts) => {
                    for h in &hosts {
                        ambiguous_env_vars
                            .entry(format!("ambiguous:{}", h))
                            .or_default()
                            .extend(names.iter().cloned());
                    }
                    return ConsumerResolution {
                        call_id: call_id.clone(),
                        service: own_service.clone(),
                        target: Some(ConsumerTarget::Unresolved {
                            reason: UnresolvedReason::EnvAmbiguous,
                            target_service: None,
                        }),
                        bound_endpoints: Vec::new(),
                        reads_complete: consumer.reads_complete,
                    };
                }
                EnvResolution::Unmapped => {
                    // The var counter is already updated; fall
                    // through to the existing
                    // `target_service_from_env` /
                    // `target_service_from_hosts` / rule-4 /
                    // rule-5 / rule-6 ladder so the operator's
                    // `services[].env` declaration still resolves
                    // the call when present. If no lower tier
                    // matches, the `env_unmapped_pending` check
                    // at the end of the function returns
                    // `Unresolved { EnvUnmapped }` (the env_sensor
                    // verdict is authoritative for
                    // `HostPart::Env` consumers).
                    env_unmapped_pending = true;
                }
            }
        }
    }
    if let Some(svc) = target_service_from_env(consumer, config) {
        let resolution = match_one_service(
            call_id,
            consumer,
            own_service,
            svc,
            &target_method,
            target_template.as_deref(),
            true,
            endpoints,
            binds,
        );
        return resolution;
    }
    if let Some(svc) = target_service_from_hosts(consumer, config) {
        let resolution = match_one_service(
            call_id,
            consumer,
            own_service,
            svc,
            &target_method,
            target_template.as_deref(),
            true,
            endpoints,
            binds,
        );
        return resolution;
    }

    // Rule 4 — external host. A `HostPart::Literal` that
    // matches no service and is not on the exempt list. Fires
    // before rule 5 because rule 4 only needs the host, not
    // the template.
    if let HostPart::Literal(host) = &consumer.url.host {
        if !host_is_external_exempt(host) {
            *external.entry(host.clone()).or_insert(0) += 1;
            return ConsumerResolution {
                call_id: call_id.clone(),
                service: own_service.clone(),
                target: Some(ConsumerTarget::External { host: host.clone() }),
                bound_endpoints: Vec::new(),
                reads_complete: consumer.reads_complete,
            };
        }
    }

    // Rule 5 — template = None AND we have not yet resolved
    // through rules 3 / 4. Per the §7.3 row order this is the
    // only path that lands a call in `unnormalized`: a
    // dynamic path with no resolvable target service and no
    // resolvable external host. A known target (rule 3) or an
    // external literal host (rule 4) preempts this row.
    if target_template.is_none() {
        return ConsumerResolution {
            call_id: call_id.clone(),
            service: own_service.clone(),
            target: Some(ConsumerTarget::Unresolved {
                reason: UnresolvedReason::Unnormalized,
                target_service: None,
            }),
            bound_endpoints: Vec::new(),
            reads_complete: consumer.reads_complete,
        };
    }

    // Rule 6 — fall back to every service except own, skipping
    // generic keys. The own-skip rule means c's own service routes
    // are not auto-resolved. Prefix-stripped matches (route_match
    // == PrefixStripped) are NOT counted as binds — per §7.4 they
    // are could-match hints surfaced through `diff::could_match`,
    // matching the rule-3 prefix-tolerance behavior above. Without
    // this gate, scenario 3's `/v1/api/orders/{}` consumer would
    // bind to orders via prefix strip instead of staying
    // Unresolved; `diff_consumers` would then key it by the
    // bound endpoint's ContractKey and miss the
    // ConsumerEndpointUnmatched change.
    let generic_keys: BTreeSet<ContractKey> = config
        .all_generic_keys()
        .into_iter()
        .filter_map(|s| parse_contract_key(&s).ok())
        .collect();
    let mut hits: Vec<ServiceName> = Vec::new();
    for ((svc, _key), providers) in endpoints {
        if svc == own_service {
            continue;
        }
        let mut strong_hit = false;
        for provider in providers {
            let pk = ContractKey::Http {
                method: MethodSpec::Known(provider.method),
                template: provider.template.clone(),
            };
            if generic_keys.contains(&pk) {
                continue;
            }
            if let Some(tmpl) = target_template.as_deref() {
                let outcome = match_route(
                    target_method.clone(),
                    tmpl,
                    provider.method,
                    &provider.template,
                );
                if let MatchOutcome::Match(detail) = outcome {
                    if detail.kind != RouteMatch::PrefixStripped {
                        strong_hit = true;
                        break;
                    }
                }
            }
        }
        if strong_hit {
            hits.push(svc.clone());
        }
    }
    hits.sort();
    hits.dedup();
    if hits.is_empty() {
        // Phase C: when the env_sensor path returned Unmapped
        // AND no lower tier found a route, the env verdict is
        // the authoritative `EnvUnmapped` (not the legacy
        // `NoMatch`). The var is already in
        // `unresolved_env_vars` for the ledger.
        let reason = if env_unmapped_pending {
            UnresolvedReason::EnvUnmapped
        } else {
            UnresolvedReason::NoMatch
        };
        return ConsumerResolution {
            call_id: call_id.clone(),
            service: own_service.clone(),
            target: Some(ConsumerTarget::Unresolved {
                reason,
                target_service: None,
            }),
            bound_endpoints: Vec::new(),
            reads_complete: consumer.reads_complete,
        };
    }
    // One hit: Binds. Several: one Binds per service. Several are
    // ambiguous and carry `Heuristic{ambiguous}` 0.3.
    let mut bound: Vec<EndpointId> = Vec::new();
    for svc in &hits {
        let best = best_provider_for(svc, &target_method, target_template.as_deref(), endpoints);
        if let Some((key, detail)) = best {
            binds.push(BindsEdge {
                consumer: call_id.clone(),
                provider: provider_node_id(endpoints, svc, &key),
                consumer_service: own_service.clone(),
                provider_service: svc.clone(),
                target_endpoint: (svc.clone(), key.clone()),
                provenance: EdgeProvenance::Heuristic {
                    detector: if hits.len() > 1 {
                        "ambiguous".into()
                    } else {
                        "unbound_host".into()
                    },
                    confidence: if hits.len() > 1 { 0.3 } else { 0.6 },
                },
                confidence: if hits.len() > 1 { 0.3 } else { 0.6 },
                route_match: detail.kind,
                stripped_prefix: detail.stripped_prefix,
            });
            bound.push((svc.clone(), key));
        }
    }
    let target = if bound.is_empty() {
        None
    } else {
        Some(ConsumerTarget::Binds {
            provenance: EdgeProvenance::Heuristic {
                detector: if hits.len() > 1 {
                    "ambiguous".into()
                } else {
                    "unbound_host".into()
                },
                confidence: if hits.len() > 1 { 0.3 } else { 0.6 },
            },
            confidence: if hits.len() > 1 { 0.3 } else { 0.6 },
            route_match: if hits.len() == 1 {
                RouteMatch::Exact
            } else {
                RouteMatch::Pattern
            },
            stripped_prefix: None,
        })
    };
    ConsumerResolution {
        call_id: call_id.clone(),
        service: own_service.clone(),
        target,
        bound_endpoints: bound,
        reads_complete: consumer.reads_complete,
    }
}

fn target_service_from_http_client(
    consumer: &crate::federation::contracts::model::ConsumerFact,
    http_clients: &CompiledHttpClients,
) -> Option<ServiceName> {
    let CallVia::Receiver { expr, fn_name, .. } = &consumer.via else {
        return None;
    };
    let combined = format!("{expr}.{fn_name}");
    for e in &http_clients.entries {
        if pattern_matches(&e.pattern, &combined) {
            // The §7.3 table says rule 3 fires whenever the
            // wrapper pattern matches; the entry's method (if any)
            // is a hint, not a filter.
            return Some(e.service.clone());
        }
    }
    None
}

/// Spec §5.3 tier 2 — code-derived base + host/env (Phase B).
/// When the call's `via` resolves to a `ClientDef` in the
/// registry, compose `base ++ call_path` and resolve the
/// composed target's host through the same `env` / `hosts`
/// matching tier 3 uses. Returns the matching `ServiceName` or
/// `None`.
///
/// The composed URL's `host` is what `target_service_from_env` and
/// `target_service_from_hosts` already know how to read — by
/// reusing them we keep the I6 total-order guarantee: tier 2 is
/// "we know the wrapper's base" plus "the base resolves to a
/// known service", not a separate heap of logic.
///
/// A consumer whose `via` is `Library { … }` (a known library
/// without registry plumbing) doesn't reach this function: tier 2
/// is a `Receiver`-only tier. Library calls fall through to
/// tier 3+ unchanged.
fn target_service_via_registry(
    consumer: &crate::federation::contracts::model::ConsumerFact,
    client_registry: &ClientRegistry,
    config: &ContractFederationConfig,
) -> Option<ServiceName> {
    if client_registry.is_empty() {
        return None;
    }
    let CallVia::Receiver { expr, fn_name, .. } = &consumer.via else {
        return None;
    };
    // Look up the receiver in the registry by `(module, name)`. The
    // module the joiner keys on is the consumer's enclosing file
    // (the `path` on the underlying `GraphNode`); the spec's
    // cross-file lookup walks one hop via `ClientDef::resolve_cross_file`.
    // The caller passes the registry the per-repo pre-pass built;
    // the joiner does not know which module owns the receiver
    // without the caller telling it. We attempt the bare-name
    // lookup first (the common case where `expr` is a module-level
    // name), and otherwise return `None` so the precedence list
    // continues at tier 3. A future orchestrator-side wiring can
    // thread the call's `module` through; spec §5.3 acceptance
    // pins the bare-name + cross-file case as the primary.
    let _ = (expr, fn_name);
    // Walk the registry for any `ClientDef` whose `name == expr`
    // and that yields a non-empty `base`. This is the joiner's
    // single-axis search: the registry keys on `(module, name)`
    // but only `name` is observable from the call's `via.expr`.
    // The orchestrator (FederatedIndex::rejoin_contracts) hands
    // the joiner a per-call filter in a future PR; for now, the
    // joiner iterates every `(module, name)` and returns the
    // first `ServiceName` the composed URL's host resolves to.
    for (_module, name, def) in client_registry.iter() {
        if name != expr {
            continue;
        }
        if def.base.is_empty() {
            continue;
        }
        // Compose base ++ call_path. The call_path comes from the
        // existing `consumer.url`; we re-derive its parts as a
        // single-Literal (the template side) plus the host part.
        let composed = compose_for_registry(consumer, def);
        let host = composed.host.clone();
        // Run the env-match helper on the composed host.
        if let Some(svc) = service_from_host(&host, config) {
            return Some(svc);
        }
    }
    None
}

// The URL / service-resolution helpers (compose_for_registry,
// service_from_host, target_service_from_env, target_service_from_hosts,
// host_matches_pattern, target_service_from_env_resolved, EnvResolution)
// live in `url_resolution.rs` (R11 partial S3 fix). The joiner
// re-exports them here for the in-file call sites.
use crate::federation::contracts::url_resolution::{
    compose_for_registry, host_matches_pattern, resolve_env_consumer, service_from_host,
    target_service_from_env, target_service_from_hosts, EnvResolution,
};

/// PR 18 — operationId fallback for generated SDK clients. Find the
/// first `(service, key)` in `endpoints` whose providers carry an
/// `operation_id == fn_name`. Returns the matched endpoint's
/// `ContractKey` and the provider node's `GlobalId`.
///
/// Determinism: iterates the `BTreeMap` in its canonical order; the
/// first match wins (operationIds are expected to be unique within
/// a service, so subsequent matches would be the same provider's
/// spec/code split).
fn find_endpoint_by_operation_id(
    service: &ServiceName,
    fn_name: &str,
    endpoints: &BTreeMap<(ServiceName, ContractKey), Vec<EndpointProviderRecord>>,
) -> Option<(ContractKey, GlobalId)> {
    for ((svc, key), providers) in endpoints {
        if svc != service {
            continue;
        }
        for p in providers {
            let op_id = match p.fact.as_ref() {
                Some(ContractFact::Provider(pf)) => pf.operation_id.as_deref(),
                _ => None,
            };
            if op_id == Some(fn_name) {
                return Some((key.clone(), p.id.clone()));
            }
        }
    }
    None
}

/// Match the consumer's `(method, template)` against every provider
/// in `target_service`. Picks the most specific match (§7.4
/// specificity). Records the `Binds` edge in `binds` and returns a
/// `ConsumerResolution`.
#[allow(clippy::too_many_arguments)]
fn match_one_service(
    call_id: &GlobalId,
    consumer: &crate::federation::contracts::model::ConsumerFact,
    own_service: &ServiceName,
    target_service: ServiceName,
    target_method: &MethodSpec,
    target_template: Option<&str>,
    rule_3: bool,
    endpoints: &BTreeMap<(ServiceName, ContractKey), Vec<EndpointProviderRecord>>,
    binds: &mut Vec<BindsEdge>,
) -> ConsumerResolution {
    let Some(template) = target_template else {
        return ConsumerResolution {
            call_id: call_id.clone(),
            service: own_service.clone(),
            target: Some(ConsumerTarget::Unresolved {
                reason: UnresolvedReason::NoRouteInService,
                target_service: Some(target_service.clone()),
            }),
            bound_endpoints: Vec::new(),
            reads_complete: consumer.reads_complete,
        };
    };
    let best = best_provider_for_in(&target_service, target_method, template, rule_3, endpoints);
    match best {
        None => {
            // PR 18 — operationId fallback. Generated SDK clients
            // (`client.orders.getOrderById({id})`) call a method
            // whose name corresponds to the OpenAPI `operationId`,
            // not the URL. When the URL match fails outright, fall
            // back to a name match. Only `CallVia::Receiver` carries
            // the SDK method identity (`fn_name`); library calls
            // (e.g. `requests.get(...)`) have no name to match.
            if let CallVia::Receiver { fn_name, .. } = &consumer.via {
                if let Some((key, provider_id)) =
                    find_endpoint_by_operation_id(&target_service, fn_name, endpoints)
                {
                    let provenance = EdgeProvenance::Heuristic {
                        detector: "operation_id".into(),
                        confidence: OPERATION_ID_CONFIDENCE,
                    };
                    binds.push(BindsEdge {
                        consumer: call_id.clone(),
                        provider: provider_id,
                        consumer_service: own_service.clone(),
                        provider_service: target_service.clone(),
                        target_endpoint: (target_service.clone(), key.clone()),
                        provenance: provenance.clone(),
                        confidence: OPERATION_ID_CONFIDENCE,
                        route_match: RouteMatch::Exact,
                        stripped_prefix: None,
                    });
                    return ConsumerResolution {
                        call_id: call_id.clone(),
                        service: own_service.clone(),
                        target: Some(ConsumerTarget::Binds {
                            provenance,
                            confidence: OPERATION_ID_CONFIDENCE,
                            route_match: RouteMatch::Exact,
                            stripped_prefix: None,
                        }),
                        bound_endpoints: vec![(target_service, key)],
                        reads_complete: consumer.reads_complete,
                    };
                }
            }
            ConsumerResolution {
                call_id: call_id.clone(),
                service: own_service.clone(),
                target: Some(ConsumerTarget::Unresolved {
                    reason: UnresolvedReason::NoRouteInService,
                    target_service: Some(target_service.clone()),
                }),
                bound_endpoints: Vec::new(),
                reads_complete: consumer.reads_complete,
            }
        }
        Some((key, detail)) => {
            // Bug C: rule-3 prefix tolerance is a could-match hint,
            // not a bind. The consumer stays Unresolved with the
            // known target service; `diff::could_match` surfaces the
            // candidate endpoint from the template prefix-strip.
            // Direct matches (no `stripped_prefix`) still bind as
            // before. Rule 6 keeps its existing suppressor at
            // `best_provider_for_in`.
            if detail.kind == RouteMatch::PrefixStripped {
                // PR 18 — operationId fallback can override a
                // prefix-stripped URL match when the SDK method
                // name matches a provider's operationId exactly.
                // The bind is still `Heuristic 0.9`, but it is exact
                // on the operationId dimension — stronger evidence
                // than a prefix-stripped URL match (which would have
                // been `Heuristic 0.5`).
                if let CallVia::Receiver { fn_name, .. } = &consumer.via {
                    if let Some((op_key, op_provider_id)) =
                        find_endpoint_by_operation_id(&target_service, fn_name, endpoints)
                    {
                        let provenance = EdgeProvenance::Heuristic {
                            detector: "operation_id".into(),
                            confidence: OPERATION_ID_CONFIDENCE,
                        };
                        binds.push(BindsEdge {
                            consumer: call_id.clone(),
                            provider: op_provider_id,
                            consumer_service: own_service.clone(),
                            provider_service: target_service.clone(),
                            target_endpoint: (target_service.clone(), op_key.clone()),
                            provenance: provenance.clone(),
                            confidence: OPERATION_ID_CONFIDENCE,
                            route_match: RouteMatch::Exact,
                            stripped_prefix: None,
                        });
                        return ConsumerResolution {
                            call_id: call_id.clone(),
                            service: own_service.clone(),
                            target: Some(ConsumerTarget::Binds {
                                provenance,
                                confidence: OPERATION_ID_CONFIDENCE,
                                route_match: RouteMatch::Exact,
                                stripped_prefix: None,
                            }),
                            bound_endpoints: vec![(target_service, op_key)],
                            reads_complete: consumer.reads_complete,
                        };
                    }
                }
                return ConsumerResolution {
                    call_id: call_id.clone(),
                    service: own_service.clone(),
                    target: Some(ConsumerTarget::Unresolved {
                        reason: UnresolvedReason::NoRouteInService,
                        target_service: Some(target_service.clone()),
                    }),
                    bound_endpoints: Vec::new(),
                    reads_complete: consumer.reads_complete,
                };
            }
            let provider = provider_node_id(endpoints, &target_service, &key);
            let (provenance, confidence) = provenance_for_detail(&detail, consumer);
            // The resolution carries the same provenance as the
            // emitted edge — §9.5 `is_certain` reads the target's
            // provenance, and §7.3 fixes rule 3 at `Static 1.0`.
            let target_provenance = provenance.clone();
            binds.push(BindsEdge {
                consumer: call_id.clone(),
                provider,
                consumer_service: own_service.clone(),
                provider_service: target_service.clone(),
                target_endpoint: (target_service.clone(), key.clone()),
                provenance,
                confidence,
                route_match: detail.kind,
                stripped_prefix: detail.stripped_prefix.clone(),
            });
            ConsumerResolution {
                call_id: call_id.clone(),
                service: own_service.clone(),
                target: Some(ConsumerTarget::Binds {
                    provenance: target_provenance,
                    confidence,
                    route_match: detail.kind,
                    stripped_prefix: detail.stripped_prefix,
                }),
                bound_endpoints: vec![(target_service, key)],
                reads_complete: consumer.reads_complete,
            }
        }
    }
}

// Removed: every call site now receives the call's owning
// service via the `own_service` parameter on `match_one_service`.

fn best_provider_for_in(
    service: &ServiceName,
    method: &MethodSpec,
    consumer_template: &str,
    rule_3: bool,
    endpoints: &BTreeMap<(ServiceName, ContractKey), Vec<EndpointProviderRecord>>,
) -> Option<(ContractKey, MatchDetail)> {
    let mut candidates: Vec<(ContractKey, MatchDetail)> = Vec::new();
    for ((svc, key), providers) in endpoints {
        if svc != service {
            continue;
        }
        for p in providers {
            let outcome = match_route(method.clone(), consumer_template, p.method, &p.template);
            if let MatchOutcome::Match(detail) = outcome {
                candidates.push((key.clone(), detail));
                break;
            }
        }
    }
    if candidates.is_empty() {
        return None;
    }
    // Pick the most specific template (§7.4 specificity).
    candidates.sort_by(|a, b| {
        let a_t = match &a.0 {
            ContractKey::Http { template, .. } => template.clone(),
            _ => String::new(),
        };
        let b_t = match &b.0 {
            ContractKey::Http { template, .. } => template.clone(),
            _ => String::new(),
        };
        compare_specificity(&a_t, &b_t).reverse()
    });
    // If `rule_3` is true the matcher already attempted prefix
    // strip. Suppress this in non-rule-3 paths by re-running without
    // it: pick the first non-PrefixStripped candidate.
    if !rule_3 {
        if let Some(non_prefix) = candidates
            .iter()
            .find(|c| c.1.kind != RouteMatch::PrefixStripped)
        {
            return Some(non_prefix.clone());
        }
    }
    Some(candidates.into_iter().next().unwrap())
}

fn best_provider_for(
    service: &ServiceName,
    method: &MethodSpec,
    consumer_template: Option<&str>,
    endpoints: &BTreeMap<(ServiceName, ContractKey), Vec<EndpointProviderRecord>>,
) -> Option<(ContractKey, MatchDetail)> {
    let tmpl = consumer_template?;
    best_provider_for_in(service, method, tmpl, false, endpoints)
}

fn provider_node_id(
    endpoints: &BTreeMap<(ServiceName, ContractKey), Vec<EndpointProviderRecord>>,
    service: &ServiceName,
    key: &ContractKey,
) -> GlobalId {
    endpoints
        .get(&(service.clone(), key.clone()))
        .and_then(|v| v.first().map(|p| p.id.clone()))
        .unwrap_or_else(|| GlobalId::from_string("unknown"))
}

fn provenance_for_detail(
    detail: &MatchDetail,
    _consumer: &crate::federation::contracts::model::ConsumerFact,
) -> (EdgeProvenance, f32) {
    match detail.kind {
        RouteMatch::PrefixStripped => (
            EdgeProvenance::Heuristic {
                detector: "prefix_stripped".into(),
                confidence: detail.confidence,
            },
            detail.confidence,
        ),
        _ if (detail.confidence - 1.0).abs() < f32::EPSILON => {
            // §7.3 rule 3: a known target service matched with the
            // real method at full confidence is `Static 1.0` — the
            // ground-truth binds (`{kind: static, confidence: 1.0}`)
            // and `is_certain` (§9.5 `Verified`) both read this
            // provenance, not a `Heuristic` labelled "static".
            (
                EdgeProvenance::Static {
                    source: crate::schema::StaticSource::TreeSitter,
                },
                detail.confidence,
            )
        }
        _ => {
            let detector = if (detail.confidence - 0.6).abs() < f32::EPSILON {
                "method_unknown"
            } else {
                "static"
            };
            (
                EdgeProvenance::Heuristic {
                    detector: detector.into(),
                    confidence: detail.confidence,
                },
                detail.confidence,
            )
        }
    }
}

/// `ContractKey::from_str` lives on the type, but the joiner needs
/// to drop a parsed `key` directly into its tables. Generic keys
/// (and `bindings[*].provider.key`) are written in the shortened
/// `<METHOD> <template>` form, not the `http:<METHOD> <template>`
/// wire form, so we normalize them here.
fn parse_contract_key(s: &str) -> Result<ContractKey, String> {
    let normalized = if s.starts_with("http:") || s.starts_with("topic:") {
        s.to_string()
    } else {
        format!("http:{s}")
    };
    normalized.parse()
}

/// Step 6 — apply confirmed bindings (§7.6).
#[allow(clippy::too_many_arguments)]
fn apply_confirmed_binding(
    binding: &ConfirmedBinding,
    binding_index: usize,
    nodes: &[GraphNode],
    assignments: &BTreeMap<String, ServiceName>,
    endpoints: &BTreeMap<(ServiceName, ContractKey), Vec<EndpointProviderRecord>>,
    binds: &mut Vec<BindsEdge>,
    consumers: &mut BTreeMap<GlobalId, ConsumerResolution>,
    stale_bindings: &mut Vec<StaleBinding>,
) {
    let provider_key = match parse_contract_key(&binding.provider.key) {
        Ok(k) => k,
        Err(_) => {
            // Malformed key (should have been caught at config load).
            return;
        }
    };
    let provider_svc = ServiceName(binding.provider.service.clone());
    let providers_for_endpoint = endpoints.get(&(provider_svc.clone(), provider_key.clone()));
    let provider_id = match providers_for_endpoint.and_then(|v| v.first().map(|p| p.id.clone())) {
        Some(id) => id,
        None => {
            stale_bindings.push(StaleBinding {
                consumer: ConfirmedBindingKey {
                    repo: RepoId::new(&binding.consumer.repo)
                        .unwrap_or_else(|_| RepoId::new("unknown").unwrap()),
                    path: binding.consumer.path.clone(),
                    symbol: binding.consumer.symbol.clone(),
                    key: binding.consumer.key.clone(),
                },
                provider: ConfirmedBindingProviderKey {
                    service: provider_svc.clone(),
                    key: binding.provider.key.clone(),
                },
                reason: StaleReason::NoEndpoint,
            });
            return;
        }
    };
    let consumer_key = match parse_contract_key(&binding.consumer.key) {
        Ok(k) => k,
        Err(_) => return,
    };
    let mut matched_any = false;
    for node in nodes {
        let Some(ContractFact::Consumer(consumer)) = node.contract.as_ref() else {
            continue;
        };
        let Ok(gid) = GlobalId::parse(&node.id) else {
            continue;
        };
        // `repo` is the repo of the call's owning node, derived from
        // its `GlobalId` (which already encodes the repo). The
        // consumer.repo is the configured one; compare strings.
        if gid.repo_id() != binding.consumer.repo {
            continue;
        }
        if node.path != binding.consumer.path {
            continue;
        }
        // Enclosing symbol: prefer `Container.name` when set,
        // otherwise the node's own name.
        let symbol_name = node.container.clone().unwrap_or_else(|| node.name.clone());
        if symbol_name != binding.consumer.symbol {
            continue;
        }
        // Build the consumer's `ContractKey`. The consumer's
        // template is `url.template`; the method is `consumer.method`.
        if consumer.url.template.is_none() {
            continue;
        }
        let tmpl = consumer.url.template.clone().unwrap();
        let ck = ContractKey::Http {
            method: consumer.method.clone(),
            template: tmpl,
        };
        if ck != consumer_key {
            continue;
        }
        matched_any = true;
        let own_svc = assignments
            .get(node.id.as_str())
            .cloned()
            .unwrap_or_else(|| ServiceName(binding.consumer.repo.clone()));
        let source = format!("repos.yaml#bindings[<{binding_index}>]");
        binds.push(BindsEdge {
            consumer: gid.clone(),
            provider: provider_id.clone(),
            consumer_service: own_svc.clone(),
            provider_service: provider_svc.clone(),
            target_endpoint: (provider_svc.clone(), provider_key.clone()),
            provenance: EdgeProvenance::Confirmed {
                source: source.clone(),
            },
            confidence: 1.0,
            route_match: RouteMatch::Exact,
            stripped_prefix: None,
        });
        consumers.insert(
            gid.clone(),
            ConsumerResolution {
                call_id: gid,
                service: own_svc,
                target: Some(ConsumerTarget::Binds {
                    provenance: EdgeProvenance::Confirmed { source },
                    confidence: 1.0,
                    route_match: RouteMatch::Exact,
                    stripped_prefix: None,
                }),
                bound_endpoints: vec![(provider_svc.clone(), provider_key.clone())],
                reads_complete: consumer.reads_complete,
            },
        );
    }
    if !matched_any {
        stale_bindings.push(StaleBinding {
            consumer: ConfirmedBindingKey {
                repo: RepoId::new(&binding.consumer.repo)
                    .unwrap_or_else(|_| RepoId::new("unknown").unwrap()),
                path: binding.consumer.path.clone(),
                symbol: binding.consumer.symbol.clone(),
                key: binding.consumer.key.clone(),
            },
            provider: ConfirmedBindingProviderKey {
                service: provider_svc.clone(),
                key: binding.provider.key.clone(),
            },
            reason: StaleReason::NoConsumer,
        });
    }
}

impl GlobalId {
    pub fn from_string(s: &str) -> Self {
        GlobalId::from_canonical(s)
    }
}

#[allow(dead_code)]
const _: () = ();
