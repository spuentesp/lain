//! Contract joiner (`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §5.3 + §7.2 /
//! §7.3 / §7.4 / §7.5 / §7.6).
//!
//! `ContractJoiner::run(nodes, edges, config) -> JoinOutput` is
//! the pure inner half of the federation's contract join. The
//! caller supplies the projected contract nodes, the projected
//! contract edges, and the validated config; the joiner returns
//! the desired `Binds` set and the `ContractIndex`. No I/O, no
//! reads from the graph, no config reads inside `run`.
//!
//! PR 7 left step 5 (§7.5 field join) as a documented no-op
//! because no `FieldRef` nodes existed yet. PR 9 fills it: the
//! joiner now collects response schemas per endpoint (from
//! `HasField` / `RequestSchema` / `ResponseSchema` edges) and
//! resolves every `FieldRef` against the endpoints its
//! `HttpClientCall` is bound to.
//!
//! ## File layout
//!
//! Pass #4 refactor (review §S3) split the original 2,362-line
//! `joiner.rs` into a `joiner/` submodule with one concern per
//! file. The split is by joiner step / concern, not by
//! visibility:
//!
//! - [`endpoints`] — step 2: build the per-service
//!   `EndpointTable` from provider nodes. Exports
//!   `build_endpoints`, `EndpointProviderRecord`, `EndpointTable`,
//!   and the supporting helpers `endpoint_template_for`,
//!   `default_broker_for`, `endpoint_id_string`.
//! - [`consumer_http`] — rule-1 gate + the §7.3 six-row ladder
//!   for HTTP consumers. Exports `resolve_consumer_to_service`
//!   (the property-test surface) and the internal
//!   `resolve_consumer`, `match_one_service`,
//!   `find_endpoint_by_operation_id`, `best_provider_for`,
//!   `provider_node_id`, `provenance_for_detail`, plus the
//!   `is_wrapper_candidate` / `registry_lookup_name` helpers
//!   used by the rule-1 gate.
//! - [`consumer_protocol`] — the three protocol resolvers
//!   (Topic, RPC, GraphQL) using the shared `resolve_by_key` +
//!   `AmbiguityPolicy` from R13 (review §S6). Exports
//!   `resolve_topic_consumer`, `resolve_rpc_consumer`,
//!   `resolve_graphql_consumer`.
//! - [`confirmed`] — step 6: apply operator-declared confirmed
//!   bindings (§7.6). Exports `apply_confirmed_binding`.
//!
//! Public symbols re-exported at this module's surface
//! (`crate::federation::contracts::joiner::X`) for back-compat
//! with the existing callers (federated_index.rs,
//! scenario_tests.rs, joiner_tests.rs, snapshots/manager.rs,
//! protocol_dispatch.rs, the property tests, and the four
//! per-protocol e2e tests in `tests/`).

use std::collections::BTreeMap;

use crate::federation::contracts::clients::ClientRegistry;
use crate::federation::contracts::config::{
    ContractFederationConfig, RoutePrefix as CfgRoutePrefix, ServiceDecl,
};
use crate::federation::contracts::field_join::{
    collect_endpoint_schemas, resolve_field_refs, EndpointSchemas,
};
use crate::federation::contracts::index::{
    ConsumerResolution, ConsumerTarget, ContractIndex, Endpoint, EndpointId, EndpointProvider,
    ServiceInfo, UnresolvedReason,
};
use crate::federation::contracts::model::{
    CallVia, ContractFact, ContractKey, Direction, HttpMethod, MethodSpec, ProviderOrigin,
    ServiceName,
};
use crate::federation::contracts::protocol_dispatch::{default_dispatch_chain, ProtocolDispatch};
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::schema::{EdgeProvenance, GraphEdge, GraphNode, RouteMatch};
use crate::server::sensors::env_sensor::EnvBindingIndex;

mod confirmed;
mod consumer_http;
mod consumer_protocol;
mod endpoints;

pub use consumer_http::resolve_consumer_to_service;
pub use consumer_protocol::{
    resolve_graphql_consumer, resolve_rpc_consumer, resolve_topic_consumer,
};
pub use endpoints::EndpointProviderRecord;

/// Spec §5.3 — I6 total-order resolution precedence, condensed
/// as a Rust enum for the joiner to surface at the per-call
/// layer.
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
/// **Total-order property (I6):** once a tier binds, no lower
/// tier can override the bind. This is the joiner's central
/// invariant — see `resolve_consumer_to_service` below for the
/// function that walks the tiers in spec order and returns at
/// the first hit.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// Tier 1 (Confirmed) or tier 2/3 with a known target
    /// service + a route that matches. Carries the originating
    /// `ServiceName` and the matching `EndpointId` (None for
    /// Confirmed bindings that don't go through the route table).
    Endpoint {
        service: ServiceName,
        endpoint: EndpointId,
        provenance: EdgeProvenance,
        confidence: f32,
        route_match: RouteMatch,
        stripped_prefix: Option<String>,
    },
    /// Tier 4 (rule 4 in §7.3): a `HostPart::Literal` that
    /// matches no service and isn't on the exempt list. Recorded
    /// in `external`; no `Binds` edge emitted.
    External { host: String },
    /// Tier 5/6: no bindable target — `Unresolved` with a
    /// reason. `target_service` carries the known service when
    /// rule 3 found one but rule 4 couldn't match a route
    /// (§7.3 row 3's `NoRouteInService`).
    Unresolved {
        reason: UnresolvedReason,
        target_service: Option<ServiceName>,
    },
}

/// The output of a `ContractJoiner::run` call. The
/// federation-level orchestrator
/// (`FederatedIndex::rejoin_contracts`) diffs the `binds` set
/// against the current `Binds` edges in the graph backend and
/// applies adds/removes through `upsert_edges_batch` +
/// `remove_edges`. The `index` is stored on a `RwLock<...>` for
/// tools.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JoinOutput {
    pub binds: Vec<BindsEdge>,
    pub index: ContractIndex,
    /// Phase C (spec §6): env vars the joiner tried to resolve
    /// via the env_sensor's bindings but had no mapping for.
    /// Keyed by var name; the count is the number of consumers
    /// that referenced the var. The orchestrator folds these
    /// into the coverage ledger as `unresolved` records with
    /// `reason: EnvUnmapped`.
    ///
    /// Key shape (pass #4 R23, review §D20): the map carries
    /// two key forms, both `String`:
    /// - `<var_name>` — the consumer's var had no env_sensor
    ///   binding at all (the env_sensor returned zero hosts).
    /// - `no_service:<host>` — every var resolved to the same
    ///   host, but no `services[].hosts` pattern matched. The
    ///   `<host>` is the resolved host (not the var name). This
    ///   key is set by
    ///   [`resolve_env_consumer`](crate::federation::contracts::url_resolution::resolve_env_consumer)
    ///   when the env path exhausts the service list.
    ///
    /// Consumers should treat both forms as `EnvUnmapped` —
    /// the difference is bookkeeping, not semantics. The
    /// `no_service:` prefix is the joiner's only way to
    /// disambiguate "the var had no binding" from "the var's
    /// binding resolved to a host no service claims".
    pub unresolved_env_vars: BTreeMap<String, u32>,
    /// Phase C (spec §6): env vars the joiner found to resolve
    /// to multiple distinct hosts in the env_sensor's bindings
    /// (`.env` says one thing, compose says another). The
    /// consumer is recorded as `Unresolved { reason:
    /// EnvAmbiguous }` and no `Binds` edge is emitted. The list
    /// of distinct hosts is kept here so the operator can
    /// disambiguate.
    pub ambiguous_env_vars: BTreeMap<String, Vec<String>>,
}

/// One desired `Binds` edge, with the join details carried on
/// it so the graph writer can set `GraphEdge.detail` and
/// `provenance` correctly. Sorted by `(consumer, provider)`.
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

/// The joiner.
pub struct ContractJoiner;

impl ContractJoiner {
    /// Compute the desired `Binds` set and the `ContractIndex`
    /// from `nodes`, `edges`, and `config`. Pure function of its
    /// inputs (§7.8).
    ///
    /// `nodes` is the slice of every contract-bearing node the
    /// federation has projected; the orchestrator pre-filters
    /// the backend list to `contract_node_ids` (per repo) to
    /// avoid a whole-graph scan (§5.3 Cost). `edges` is the
    /// matching slice of `ReadsField`, `ReadsFrom`, `HasField`,
    /// `RequestSchema`, and `ResponseSchema` edges — the joiner
    /// reads only these to fill the §7.5 step. Other edge types
    /// in `edges` are ignored.
    pub fn run(
        nodes: &[GraphNode],
        edges: &[GraphEdge],
        config: &ContractFederationConfig,
    ) -> JoinOutput {
        Self::run_with_registry(nodes, edges, config, &ClientRegistry::new())
    }

    /// Phase B (spec §5.1 / §5.3 tier 2): same as [`run`] but
    /// the caller passes the per-repo client registry the
    /// cross-file pre-pass built. An empty registry is
    /// equivalent to [`run`] (no tier-2 resolution). The
    /// orchestrator (`FederatedIndex::rejoin_contracts`) is the
    /// primary caller; the property tests in
    /// `tests/property_join_pipeline.rs` and the acceptance
    /// tests in `tests/wrapper_resolution.rs` use this surface
    /// directly.
    pub fn run_with_registry(
        nodes: &[GraphNode],
        edges: &[GraphEdge],
        config: &ContractFederationConfig,
        registry: &ClientRegistry,
    ) -> JoinOutput {
        Self::run_with_registry_and_env(nodes, edges, config, registry, &EnvBindingIndex::default())
    }

    /// Phase C (spec §6): same as [`run_with_registry`] but
    /// the caller passes the per-repo env-var bindings the
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
        // Step 1 — assign services to every node (§4.1).
        let assignments = assign_services(nodes, config);

        // Step 2 — build endpoints (§7.2).
        let endpoint_table = endpoints::build_endpoints(nodes, &assignments, config);

        // Step 3 — filter wrapper candidates (§7.3 rule 1).
        let http_clients = compile_http_clients(config);

        let client_registry = registry;
        let env_index = env;

        let mut unresolved_env_vars: BTreeMap<String, u32> = BTreeMap::new();
        let mut ambiguous_env_vars: BTreeMap<String, Vec<String>> = BTreeMap::new();

        // Step 4 — resolve consumers (§7.3 table).
        let mut consumers: BTreeMap<GlobalId, ConsumerResolution> = BTreeMap::new();
        let mut external: BTreeMap<String, u32> = BTreeMap::new();
        let mut unnormalized: Vec<GlobalId> = Vec::new();
        let mut binds: Vec<BindsEdge> = Vec::new();
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
            // Rule 1 — Phase A rule-1 fix. Wrapper candidates
            // with no matching `http_clients` entry are NOT
            // silently discarded (the pre-Phase-A bug). Instead,
            // they are recorded as
            // `Unresolved { reason: WrapperUnconfigured }` so
            // the coverage ledger's `unresolved` bucket counts
            // them and `evaluate()` can downgrade
            // `NoKnownImpact` on a `CouldMatch` verdict
            // (§9.5, §9.7). Rule 2 (confirmed bindings) is
            // applied in step 6 below.
            //
            // When `http_clients` is empty (no operator
            // config), the pre-Phase-A `continue` is preserved
            // UNLESS the per-repo client registry
            // (`client_registry`, Phase B §5.1) has a
            // `ClientDef` for this receiver — in that case
            // the joiner's tier-2 path (spec §5.3) has a
            // known base and the operator's expectation is
            // that the receiver resolves. The registry is the
            // cross-file client pre-pass output; the joiner
            // consults it before dropping.
            if consumer_http::is_wrapper_candidate(consumer) {
                let CallVia::Receiver { ref expr, .. } = consumer.via else {
                    continue;
                };
                let registry_has_def =
                    consumer_http::registry_lookup_name(client_registry, expr).is_some();
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
            let own_service = assignments
                .get(call_id.as_str())
                .cloned()
                .unwrap_or_else(|| implicit_service(node));
            let resolution = consumer_http::resolve_consumer(
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

        // Step 6 — apply confirmed bindings (§7.6).
        let mut stale_bindings = Vec::new();
        for (idx, binding) in config.bindings.iter().enumerate() {
            confirmed::apply_confirmed_binding(
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
        let field_ref_nodes: Vec<&GraphNode> = nodes
            .iter()
            .filter(|n| {
                n.node_type == crate::schema::NodeType::FieldRef
                    && matches!(n.contract.as_ref(), Some(ContractFact::FieldRead(_)))
            })
            .collect();
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
        binds.extend(field_binds);

        // Step 7 — sort every output collection by its key.
        binds.sort_by(|a, b| {
            a.consumer
                .as_str()
                .cmp(b.consumer.as_str())
                .then_with(|| a.provider.as_str().cmp(b.provider.as_str()))
        });
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
            endpoint_ids.sort_by_key(endpoints::endpoint_id_string);
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
            let endpoint_id = (service.clone(), key.clone());
            let mut endpoint_schemas: BTreeMap<
                Direction,
                crate::federation::contracts::index::EndpointSchema,
            > = BTreeMap::new();
            if let Some(by_dir) = schemas.by_endpoint.get(&endpoint_id) {
                for (dir, fields) in by_dir {
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

/// Step 1: assign a service to every node. The longest
/// matching `paths` prefix wins; ties go to the first service
/// in declaration order. Nodes whose `repo_id` is `None`
/// (single-workspace graphs) use the implicit service name =
/// repo id from the node's `GlobalId::repo_id`.
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
    /// without a `ContractFederationConfig`; the property tests
    /// in `tests/property_join_pipeline.rs` exercise the
    /// joiner's I2 / I5 / I6 total-order invariants without an
    /// orchestrator.
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

    fn matches(&self, via: &CallVia) -> bool {
        let CallVia::Receiver { expr, fn_name, .. } = via else {
            return false;
        };
        let combined = format!("{expr}.{fn_name}");
        self.entries
            .iter()
            .any(|e| pattern_matches(&e.pattern, &combined))
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
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

fn pattern_matches(pattern: &str, fn_name: &str) -> bool {
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

pub(super) fn parse_method(s: &str) -> Option<HttpMethod> {
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

/// `ContractKey::from_str` lives on the type, but the joiner
/// needs to drop a parsed `key` directly into its tables.
/// Generic keys (and `bindings[*].provider.key`) are written in
/// the shortened `<METHOD> <template>` form, not the
/// `http:<METHOD> <template>` wire form, so we normalize them
/// here.
pub(super) fn parse_contract_key(s: &str) -> Result<ContractKey, String> {
    let normalized = if s.starts_with("http:") || s.starts_with("topic:") {
        s.to_string()
    } else {
        format!("http:{s}")
    };
    normalized.parse()
}

/// Provenance for the §7.3 rule-6 heuristic Binds. One hit
/// means the joiner selected a single service via the
/// unbound-host path (`"unbound_host"`, 0.6); several hits means
/// the same path returned multiple services and the joiner
/// bound them all as ambiguous (`"ambiguous"`, 0.3). Single
/// source of truth so the per-edge `EdgeProvenance::Heuristic`
/// and the `ConsumerTarget::Binds` provenance can't drift
/// apart (Phase B-D review §S7, §D17).
pub(super) fn heuristic_provenance_for(hit_count: usize) -> (&'static str, f32) {
    if hit_count > 1 {
        ("ambiguous", 0.3)
    } else {
        ("unbound_host", 0.6)
    }
}

impl GlobalId {
    pub fn from_string(s: &str) -> Self {
        GlobalId::from_canonical(s)
    }
}

#[allow(dead_code)]
const _: () = ();
