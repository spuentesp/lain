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
    CallVia, ContractFact, ContractKey, Direction, HostPart, HttpMethod, MethodSpec, ProviderFact,
    ProviderOrigin, ServiceName,
};
use crate::federation::contracts::route_match::{
    compare_specificity, match_route, MatchDetail, MatchOutcome,
};
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::schema::{EdgeProvenance, GraphEdge, GraphNode, RouteMatch};

/// The output of a `ContractJoiner::run` call. The federation-level
/// orchestrator (`FederatedIndex::rejoin_contracts`) diffs the
/// `binds` set against the current `Binds` edges in the graph backend
/// and applies adds/removes through `upsert_edges_batch` +
/// `remove_edges`. The `index` is stored on a `RwLock<...>` for tools.
#[derive(Debug, Clone, PartialEq)]
pub struct JoinOutput {
    pub binds: Vec<BindsEdge>,
    pub index: ContractIndex,
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
        // Step 1 — assign services to every node (§4.1). Longest
        // matching prefix wins; implicit service = repo id.
        let assignments = assign_services(nodes, config);

        // Step 2 — build endpoints (§7.2). Group provider nodes by
        // (service, method, template_after_prefixes). Code +
        // OpenAPI merge into one endpoint with two providers.
        let endpoint_table = build_endpoints(nodes, &assignments, config);

        // Step 3 — filter wrapper candidates (§7.3 rule 1).
        let http_clients = compile_http_clients(config);

        // Step 4 — resolve consumers (§7.3 table).
        let mut consumers: BTreeMap<GlobalId, ConsumerResolution> = BTreeMap::new();
        let mut external: BTreeMap<String, u32> = BTreeMap::new();
        let mut unnormalized: Vec<GlobalId> = Vec::new();
        let mut binds: Vec<BindsEdge> = Vec::new();
        for node in nodes {
            let Some(ContractFact::Consumer(consumer)) = node.contract.as_ref() else {
                continue;
            };
            let call_id = match GlobalId::parse(&node.id) {
                Ok(g) => g,
                Err(_) => continue,
            };
            // Rule 1 — wrapper candidates with no matching
            // http_clients entry are discarded (rule 1 of the
            // §7.3 table). Rule 2 (confirmed bindings) is
            // applied in step 6 below.
            if is_wrapper_candidate(consumer) && !http_clients.matches(&consumer.via) {
                continue;
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
        let (field_refs, _schemaless, _unknown) =
            resolve_field_refs(&field_ref_nodes_owned, &reads_from_edges, &binds, &schemas);
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
            };
            let template = match &key {
                ContractKey::Http { template, .. } => template.clone(),
                ContractKey::Topic { name, .. } => name.clone(),
            };
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
                    // Pick the schema node id of the first provider
                    // for this endpoint — the openapi sensor emits one
                    // Schema per direction per operation; the joiner
                    // does not need to differentiate between them.
                    let schema_node_id = provider_records
                        .first()
                        .map(|p| p.node_id.clone())
                        .unwrap_or_else(|| GlobalId::from_string("unknown"));
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
        JoinOutput { binds, index }
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
struct EndpointProviderRecord {
    id: GlobalId,
    fact: Option<ContractFact>,
    template: String,
    method: HttpMethod,
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
/// merge into one endpoint with two providers.
fn build_endpoints(
    nodes: &[GraphNode],
    assignments: &BTreeMap<String, ServiceName>,
    config: &ContractFederationConfig,
) -> BTreeMap<(ServiceName, ContractKey), Vec<EndpointProviderRecord>> {
    let mut table: BTreeMap<(ServiceName, ContractKey), Vec<EndpointProviderRecord>> =
        BTreeMap::new();
    for node in nodes {
        let Some(ContractFact::Provider(provider)) = node.contract.as_ref() else {
            continue;
        };
        let Ok(gid) = GlobalId::parse(&node.id) else {
            continue;
        };
        let svc = match assignments.get(node.id.as_str()) {
            Some(s) => s,
            None => continue,
        };
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
    table
}

fn is_wrapper_candidate(consumer: &crate::federation::contracts::model::ConsumerFact) -> bool {
    matches!(consumer.via, CallVia::Receiver { .. })
}

#[derive(Debug, Clone, Default)]
struct CompiledHttpClients {
    entries: Vec<CompiledHttpClient>,
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
        let CallVia::Receiver { expr, fn_name } = via else {
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

/// Resolve one consumer. Returns a `ConsumerResolution` and appends
/// any `Binds` edges it produced. The 6-row §7.3 table runs in
/// order; the first match wins.
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
) -> ConsumerResolution {
    let target_template = consumer.url.template.clone();
    let target_method = consumer.method.clone();

    // Rule 3 — target service known. Check http_clients first,
    // then env, then hosts. Fires even when `template = None`:
    // a known target with a dynamic path becomes
    // `unresolved no_route_in_service` (rule 3's verdict),
    // not `unnormalized` (rule 5). Per §7.3 the first matching
    // row decides; rule 5 only fires when rule 3 found no
    // target service.
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
            }),
            bound_endpoints: Vec::new(),
            reads_complete: consumer.reads_complete,
        };
    }

    // Rule 6 — fall back to every service except own, skipping
    // generic keys. The own-skip rule means c's own service routes
    // are not auto-resolved.
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
                if outcome.is_match() {
                    hits.push(svc.clone());
                    break;
                }
            }
        }
    }
    hits.sort();
    hits.dedup();
    if hits.is_empty() {
        return ConsumerResolution {
            call_id: call_id.clone(),
            service: own_service.clone(),
            target: Some(ConsumerTarget::Unresolved {
                reason: UnresolvedReason::NoMatch,
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
    let CallVia::Receiver { expr, fn_name } = &consumer.via else {
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

fn target_service_from_env(
    consumer: &crate::federation::contracts::model::ConsumerFact,
    config: &ContractFederationConfig,
) -> Option<ServiceName> {
    if let HostPart::Env(envs) = &consumer.url.host {
        for env in envs {
            for s in &config.services {
                if s.env.iter().any(|e| e == env) {
                    return Some(ServiceName(s.name.clone()));
                }
            }
        }
    }
    None
}

fn target_service_from_hosts(
    consumer: &crate::federation::contracts::model::ConsumerFact,
    config: &ContractFederationConfig,
) -> Option<ServiceName> {
    let HostPart::Literal(host) = &consumer.url.host else {
        return None;
    };
    for s in &config.services {
        if s.hosts.iter().any(|h| host_matches_pattern(h, host)) {
            return Some(ServiceName(s.name.clone()));
        }
    }
    None
}

fn host_matches_pattern(pattern: &str, host: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("*.") {
        host == suffix || host.ends_with(&format!(".{suffix}"))
    } else {
        pattern == host
    }
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
            }),
            bound_endpoints: Vec::new(),
            reads_complete: consumer.reads_complete,
        };
    };
    let best = best_provider_for_in(&target_service, target_method, template, rule_3, endpoints);
    match best {
        None => ConsumerResolution {
            call_id: call_id.clone(),
            service: own_service.clone(),
            target: Some(ConsumerTarget::Unresolved {
                reason: UnresolvedReason::NoRouteInService,
            }),
            bound_endpoints: Vec::new(),
            reads_complete: consumer.reads_complete,
        },
        Some((key, detail)) => {
            let provider = provider_node_id(endpoints, &target_service, &key);
            let (provenance, confidence) = provenance_for_detail(&detail, consumer);
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
                    provenance: EdgeProvenance::Heuristic {
                        detector: detector_for(&detail),
                        confidence,
                    },
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

fn detector_for(detail: &MatchDetail) -> String {
    match detail.kind {
        RouteMatch::PrefixStripped => "prefix_stripped".into(),
        _ => {
            if (detail.confidence - 0.6).abs() < f32::EPSILON {
                "method_unknown".into()
            } else {
                "static".into()
            }
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
