//! Step 6 of the joiner — apply confirmed bindings (§7.6).
//!
//! A confirmed binding (declared in `repos.yaml#bindings[]`)
//! replaces the rule-4 verdict for every matching call. Stale
//! entries (binding references a deleted endpoint or no
//! matching consumer exists) are recorded on the
//! `JoinOutput.stale_bindings` list for the surface tools.

use std::collections::BTreeMap;

use crate::federation::contracts::config::ConfirmedBinding;
use crate::federation::contracts::index::{
    ConfirmedBindingKey, ConfirmedBindingProviderKey, ConsumerResolution, ConsumerTarget,
    StaleBinding, StaleReason,
};
use crate::federation::contracts::joiner::endpoints::EndpointTable;
use crate::federation::contracts::joiner::parse_contract_key;
use crate::federation::contracts::joiner::BindsEdge;
use crate::federation::contracts::model::{ContractFact, ContractKey, ServiceName};
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::schema::{EdgeProvenance, GraphNode, RouteMatch};

/// Apply one confirmed binding to the current bind set. The
/// orchestrator calls this once per entry in
/// `config.bindings`. Returns nothing; mutates `binds`,
/// `consumers`, and `stale_bindings` in place.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_confirmed_binding(
    binding: &ConfirmedBinding,
    binding_index: usize,
    nodes: &[GraphNode],
    assignments: &BTreeMap<String, ServiceName>,
    endpoints: &EndpointTable,
    binds: &mut Vec<BindsEdge>,
    consumers: &mut BTreeMap<GlobalId, ConsumerResolution>,
    stale_bindings: &mut Vec<StaleBinding>,
) {
    let provider_key = match parse_contract_key(&binding.provider.key) {
        Ok(k) => k,
        Err(_) => return,
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
