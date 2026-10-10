//! HTTP-side consumer resolution — rule-1 gate + §7.3 ladder.
//!
//! [`resolve_consumer`] is the §7.3 row-by-row walk; the six
//! tiers run in spec order, the first match wins. Tier 1
//! (confirmed bindings) is handled separately by
//! [`super::confirmed::apply_confirmed_binding`] before this
//! function is called.
//!
//! Spec §5.3 / I6 total-order precedence:
//!
//! 1. Confirmed binding — `apply_confirmed_binding` (§7.6).
//! 2. Code-derived base + host/env (Phase B [`ClientRegistry`] —
//!    composes `base ++ call_path` via
//!    [`crate::federation::contracts::clients::compose_and_normalize`]).
//! 3. `http_clients` config pattern (rule 3 first branch in §7.3).
//! 4. operationId match (Heuristic 0.9, PR 18 — runs inside
//!    `match_one_service` after a URL match attempt fails).
//! 5. Unbound-host heuristic (0.6) / ambiguous (0.3) (rule 6 in
//!    §7.3).
//! 6. Unresolved candidate (rule 5 in §7.3 when template is
//!    `None`; otherwise the reason recorded on the verdict).
//!
//! `client_registry` is the Phase B wrapper-base registry. An
//! empty registry skips tier 2 entirely (every existing pre-Phase
//! B test passes an empty registry, preserving the §7.3 row
//! order).
//!
//! Public alias: [`resolve_consumer_to_service`] (same call
//! shape + same I6 total order, exposed at the joiner module's
//! surface so the property tests in
//! `tests/property/join_pipeline.rs` can pin I2 / I5 / I6 without
//! round-tripping through the full `ContractJoiner::run`
//! orchestrator).

use std::collections::{BTreeMap, BTreeSet};

use crate::federation::contracts::clients::ClientRegistry;
use crate::federation::contracts::config::ContractFederationConfig;
use crate::federation::contracts::index::{
    ConsumerResolution, ConsumerTarget, EndpointId, UnresolvedReason,
};
use crate::federation::contracts::joiner::endpoints::EndpointTable;
use crate::federation::contracts::joiner::BindsEdge;
use crate::federation::contracts::model::{
    CallVia, ConsumerFact, ContractFact, ContractKey, HostPart, MethodSpec, ServiceName,
};
use crate::federation::contracts::route_match::{
    compare_specificity, match_route, MatchDetail, MatchOutcome,
};
use crate::federation::contracts::url_resolution::{
    resolve_env_consumer, service_from_host, target_service_from_env, target_service_from_hosts,
    EnvResolution,
};
use crate::federation::repo_id::GlobalId;
use crate::schema::{EdgeProvenance, RouteMatch};
use crate::server::sensors::env_sensor::EnvBindingIndex;

/// Confidence assigned to an `operationId`-only bind (PR 18).
///
/// Higher than prefix-stripped (0.5) and unbound-host (0.6); lower
/// than the rule-3 URL match (Static 1.0). OperationIds are unique
/// within an OpenAPI spec, so the name match is strong evidence, but
/// the URL does not actually match, so it cannot be `Static`. Per
/// §4.6, a `Heuristic` provenance does not count as "certain" for
/// `Verified` — keeping room for a `NeedsInvestigation` verdict when
/// the bind is used in a critical change.
pub(super) const OPERATION_ID_CONFIDENCE: f32 = 0.9;

/// Public alias: single-call surface that property tests use to
/// pin I2 / I5 / I6 without round-tripping through the full
/// `ContractJoiner::run` orchestrator. The synthetic `call_id`
/// minted from the consumer's `url_expr` keeps the resolution's
/// `call_id` deterministic across runs (I4).
#[allow(clippy::too_many_arguments)]
pub fn resolve_consumer_to_service(
    consumer: &ConsumerFact,
    own_service: &ServiceName,
    config: &ContractFederationConfig,
    endpoints: &EndpointTable,
    http_clients: &super::CompiledHttpClients,
    external: &mut BTreeMap<String, u32>,
    binds: &mut Vec<BindsEdge>,
    client_registry: &ClientRegistry,
) -> ConsumerResolution {
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

/// Resolve one consumer. Returns a `ConsumerResolution` and
/// appends any `Binds` edges it produced. The 6-row §7.3 table
/// runs in order; the first match wins.
///
/// `client_registry` is the Phase B wrapper-base registry. An
/// empty registry skips tier 2 entirely.
///
/// `env` is the Phase C env_sensor's per-repo index; the
/// `unresolved_env_vars` / `ambiguous_env_vars` accumulators are
/// filled in when a `HostPart::Env([var])` consumer has no binding
/// or binds to conflicting hosts.
#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_consumer(
    call_id: &GlobalId,
    consumer: &crate::federation::contracts::model::ConsumerFact,
    own_service: &ServiceName,
    config: &ContractFederationConfig,
    http_clients: &super::CompiledHttpClients,
    endpoints: &EndpointTable,
    binds: &mut Vec<BindsEdge>,
    external: &mut BTreeMap<String, u32>,
    client_registry: &ClientRegistry,
    env: &EnvBindingIndex,
    unresolved_env_vars: &mut BTreeMap<String, u32>,
    ambiguous_env_vars: &mut BTreeMap<String, Vec<String>>,
) -> ConsumerResolution {
    let target_template = consumer.url.template.clone();
    let target_method = consumer.method.clone();
    let mut env_unmapped_pending = false;

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
    if let HostPart::Env(names) = &consumer.url.host {
        if let Some(resolved) = resolve_env_consumer(consumer, env, config, unresolved_env_vars) {
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

    let generic_keys: BTreeSet<ContractKey> = config
        .all_generic_keys()
        .into_iter()
        .filter_map(|s| super::parse_contract_key(&s).ok())
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
    let (detector, confidence) = super::heuristic_provenance_for(hits.len());
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
                    detector: detector.into(),
                    confidence,
                },
                confidence,
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
                detector: detector.into(),
                confidence,
            },
            confidence,
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
    http_clients: &super::CompiledHttpClients,
) -> Option<ServiceName> {
    let CallVia::Receiver { expr, fn_name, .. } = &consumer.via else {
        return None;
    };
    let combined = format!("{expr}.{fn_name}");
    for e in &http_clients.entries {
        if super::pattern_matches(&e.pattern, &combined) {
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
fn target_service_via_registry(
    consumer: &crate::federation::contracts::model::ConsumerFact,
    client_registry: &ClientRegistry,
    config: &ContractFederationConfig,
) -> Option<ServiceName> {
    use crate::federation::contracts::clients::{compose_and_normalize, UrlPart};
    if client_registry.is_empty() {
        return None;
    }
    let CallVia::Receiver { expr, fn_name, .. } = &consumer.via else {
        return None;
    };
    let _ = (expr, fn_name);
    for (_module, name, def) in client_registry.iter() {
        if name != expr {
            continue;
        }
        if def.base.is_empty() {
            continue;
        }
        // Inlined from `url_resolution::compose_for_registry`
        // (pass #4 R20, review §D15): render the consumer's
        // template as a single Literal part and prepend
        // `def.base`, then normalize. The path_part is `/` when
        // the template is empty so the composed URL is
        // well-formed (no trailing template + an empty path).
        let template = consumer.url.template.clone().unwrap_or_default();
        let path_part = if template.is_empty() {
            UrlPart::Literal("/".into())
        } else {
            UrlPart::Literal(template)
        };
        let composed = compose_and_normalize(&[path_part], &def.base);
        let host = composed.host.clone();
        if let Some(svc) = service_from_host(&host, config) {
            return Some(svc);
        }
    }
    None
}

fn host_is_external_exempt(host: &str) -> bool {
    matches!(
        host,
        "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]" | "host.docker.internal"
    )
}

/// Match the consumer's `(method, template)` against every
/// provider in `target_service`. Picks the most specific match
/// (§7.4 specificity). Records the `Binds` edge in `binds` and
/// returns a `ConsumerResolution`.
#[allow(clippy::too_many_arguments)]
fn match_one_service(
    call_id: &GlobalId,
    consumer: &crate::federation::contracts::model::ConsumerFact,
    own_service: &ServiceName,
    target_service: ServiceName,
    target_method: &MethodSpec,
    target_template: Option<&str>,
    rule_3: bool,
    endpoints: &EndpointTable,
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
            if detail.kind == RouteMatch::PrefixStripped {
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

/// PR 18 — operationId fallback for generated SDK clients. Find
/// the first `(service, key)` in `endpoints` whose providers
/// carry an `operation_id == fn_name`. Returns the matched
/// endpoint's `ContractKey` and the provider node's `GlobalId`.
///
/// Determinism: iterates the `BTreeMap` in its canonical order;
/// the first match wins (operationIds are expected to be unique
/// within a service, so subsequent matches would be the same
/// provider's spec/code split).
fn find_endpoint_by_operation_id(
    service: &ServiceName,
    fn_name: &str,
    endpoints: &EndpointTable,
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

fn best_provider_for_in(
    service: &ServiceName,
    method: &MethodSpec,
    consumer_template: &str,
    rule_3: bool,
    endpoints: &EndpointTable,
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
    endpoints: &EndpointTable,
) -> Option<(ContractKey, MatchDetail)> {
    let tmpl = consumer_template?;
    best_provider_for_in(service, method, tmpl, false, endpoints)
}

fn provider_node_id(
    endpoints: &EndpointTable,
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
        _ if (detail.confidence - 1.0).abs() < f32::EPSILON => (
            EdgeProvenance::Static {
                source: crate::schema::StaticSource::TreeSitter,
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

pub(super) fn is_wrapper_candidate(
    consumer: &crate::federation::contracts::model::ConsumerFact,
) -> bool {
    matches!(consumer.via, CallVia::Receiver { .. })
}

/// Phase B (spec §5.1 + §5.3 tier 2): does the registry carry
/// any `ClientDef` whose `name == query_name`? A hit means the
/// joiner can compose `base ++ call_path` for this receiver —
/// the rule-1 gate must NOT drop the call.
pub(super) fn registry_lookup_name<'a>(
    registry: &'a ClientRegistry,
    query_name: &str,
) -> Option<&'a crate::federation::contracts::clients::ClientDef> {
    registry
        .iter()
        .find(|(_, name, _)| *name == query_name)
        .map(|(_, _, def)| def)
}
