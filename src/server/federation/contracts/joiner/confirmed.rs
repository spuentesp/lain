//! Step 6 of the joiner — apply confirmed bindings (§7.6).
//!
//! A confirmed binding (declared in `repos.yaml#bindings[]`)
//! replaces the rule-4 verdict for every matching call. A binding
//! applies only when its `consumer.key` and `provider.key` form a
//! well-formed pair in the `check_binding` sense
//! (`key_compat::keys_compatible`) — I1a ("no invented bindings")
//! forbids pinning a consumer to an endpoint of a different kind
//! or with incompatible methods/templates. Same-kind HTTP template
//! bridges (e.g. `GET /v1/api/orders/{}` → `GET /api/orders/{}`)
//! are supported. Stale entries (binding references a deleted
//! endpoint, no matching consumer exists, or the keys are
//! ill-formed) are recorded on the `JoinOutput.stale_bindings`
//! list for the surface tools.

use std::collections::BTreeMap;

use crate::federation::contracts::config::ConfirmedBinding;
use crate::federation::contracts::index::{
    ConfirmedBindingKey, ConfirmedBindingProviderKey, ConsumerResolution, ConsumerTarget,
    StaleBinding, StaleReason,
};
use crate::federation::contracts::joiner::endpoints::EndpointTable;
use crate::federation::contracts::joiner::parse_contract_key;
use crate::federation::contracts::joiner::BindsEdge;
use crate::federation::contracts::key_compat::keys_compatible;
use crate::federation::contracts::model::{ContractFact, ContractKey, ServiceName};
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::schema::{EdgeProvenance, GraphNode, RouteMatch};

/// Apply one confirmed binding to the current bind set. The
/// orchestrator calls this once per entry in
/// `config.bindings`. Returns nothing; mutates `binds`,
/// `consumers`, and `stale_bindings` in place. The binding applies
/// only when `consumer.key` and `provider.key` form a well-formed
/// pair (`key_compat::keys_compatible` — `check_binding`'s rules;
/// same-kind HTTP template bridges included). An ill-formed pair is
/// recorded as a `StaleReason::IllFormedKeys` stale entry and binds
/// nothing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_confirmed_binding(
    binding: &ConfirmedBinding,
    binding_index: usize,
    nodes: &[GraphNode],
    assignments: &BTreeMap<String, ServiceName>,
    endpoints: &EndpointTable,
    binds: &mut Vec<BindsEdge>,
    consumers: &mut BTreeMap<GlobalId, Vec<ConsumerResolution>>,
    stale_bindings: &mut Vec<StaleBinding>,
) {
    let provider_key = match parse_contract_key(&binding.provider.key) {
        Ok(k) => k,
        Err(_) => return,
    };
    let consumer_key = match parse_contract_key(&binding.consumer.key) {
        Ok(k) => k,
        Err(_) => return,
    };
    // I1a (docs/formal/ConsumerBinding.tla): a confirmed binding
    // may only bind a consumer to an endpoint whose key is
    // compatible with the consumer's key — `check_binding`'s rules
    // (`key_compat::keys_compatible`): same kind; for HTTP,
    // compatible methods and templates (so a `GET /v1/…` →
    // `GET /api/…` bridge is fine); for GraphQL, equal `(op,
    // field)`. Cross-kind pairs (e.g. `graphql:query:product` →
    // `http:POST /graphql`) invent a binding and bind nothing;
    // `validate()` rejects the same entry at load.
    if !keys_compatible(&consumer_key, &provider_key) {
        stale_bindings.push(StaleBinding {
            consumer: ConfirmedBindingKey {
                repo: RepoId::new(&binding.consumer.repo)
                    .unwrap_or_else(|_| RepoId::new("unknown").unwrap()),
                path: binding.consumer.path.clone(),
                symbol: binding.consumer.symbol.clone(),
                key: binding.consumer.key.clone(),
            },
            provider: ConfirmedBindingProviderKey {
                service: ServiceName(binding.provider.service.clone()),
                key: binding.provider.key.clone(),
            },
            reason: StaleReason::IllFormedKeys,
        });
        return;
    }
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
    let mut matched_any = false;
    for node in nodes {
        // Every consumer-side key the node carries — a
        // `topic-read:<path>:<line>` site can subscribe to several
        // topics, and a `TableConsumerFact` lists several tables, so
        // the binding's key may match any of them (not only the
        // first fact). HTTP / GraphQL / Topic / Rpc / WebSocket /
        // Table all map through `ContractFact::consumer_keys`. Each
        // key travels with its own fact's `reads_complete`: the §6.5
        // escape flag lives on `Consumer` (HTTP) only — every other
        // consumer kind is recorded fully read — and a mixed node
        // (an HTTP call that escapes tracking plus a topic
        // subscription) must not inherit the wrong fact's flag.
        let mut node_keys: Vec<(ContractKey, bool)> = Vec::new();
        for fact in &node.contract {
            let reads_complete = match fact {
                ContractFact::Consumer(consumer) => consumer.reads_complete,
                _ => true,
            };
            for key in fact.consumer_keys() {
                node_keys.push((key, reads_complete));
            }
        }
        if node_keys.is_empty() {
            continue;
        }
        let Ok(gid) = GlobalId::parse(&node.id) else {
            continue;
        };
        if gid.repo_id() != binding.consumer.repo {
            continue;
        }
        if node.path != binding.consumer.path {
            continue;
        }
        let symbol_name = node.container.clone().unwrap_or_else(|| node.name.clone());
        if symbol_name != binding.consumer.symbol {
            continue;
        }
        let Some((_, reads_complete)) = node_keys.iter().find(|(k, _)| k == &consumer_key) else {
            continue;
        };
        let reads_complete = *reads_complete;
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
        // F3 (multi-fact consumers merge-into-vec): the node may
        // carry more than one consumer fact — `topic-read:<path>:<line>`
        // subscribes to every topic on its line, a synthetic
        // `sql-read:` node reads every table in its statement, a
        // mixed node may carry an HTTP `Consumer` plus a topic
        // subscription. The joiner pre-pass already built one
        // `ConsumerResolution` per fact in `consumers[gid]`; an
        // operator confirmation must override ONLY the resolution
        // whose key this binding names — the others stay.
        //
        // We keep every existing resolution whose target is NOT a
        // `Binds` whose `bound_endpoints` names the same
        // `provider_key` we're about to confirm (the slot the
        // binding covers; any prior confirmed resolution of the
        // same shape is overwritten). The new confirmed resolution
        // is inserted at the FRONT — `consumer(&gid)` returns the
        // first resolution (`index.rs:60`), and an operator
        // confirmation is the primary truth for that key.
        //
        // `binds` does not need this — it accumulates per-binding
        // without dedup, so multi-binding matches on the same node
        // correctly grow the edge set.
        let new_resolution = ConsumerResolution {
            call_id: gid.clone(),
            service: own_svc,
            target: Some(ConsumerTarget::Binds {
                provenance: EdgeProvenance::Confirmed { source },
                confidence: 1.0,
                route_match: RouteMatch::Exact,
                stripped_prefix: None,
            }),
            bound_endpoints: vec![(provider_svc.clone(), provider_key.clone())],
            reads_complete,
        };
        let entry = consumers.entry(gid).or_default();
        entry.retain(|existing| {
            !matches!(
                &existing.target,
                Some(ConsumerTarget::Binds { .. })
                    if existing
                        .bound_endpoints
                        .first()
                        .map(|e| &e.1 == &provider_key)
                        .unwrap_or(false)
            )
        });
        entry.insert(0, new_resolution);
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
