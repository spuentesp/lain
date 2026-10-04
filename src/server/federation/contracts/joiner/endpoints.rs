//! Step 2 of the joiner — build the per-service `EndpointTable`.
//!
//! Spec §7.2: provider nodes (`ContractFact::Provider`,
//! `RpcProvider`, `GraphqlProvider`) are grouped by
//! `(service, method, template_after_prefixes)`. A code route and
//! an OpenAPI operation with the same triple merge into one
//! endpoint with two providers. Topic providers (§6.7) are keyed
//! on `(service, ContractKey::Topic { broker, name })`. RPC
//! providers (§8.2) are keyed on
//! `(service, ContractKey::Rpc { system, service, method })`.
//! GraphQL providers (§8.3) are keyed on
//! `(service, ContractKey::Graphql { op, field })`.

use std::collections::BTreeMap;

use crate::federation::contracts::config::{ContractFederationConfig, ServiceDecl};
use crate::federation::contracts::index::EndpointId;
use crate::federation::contracts::model::{
    ContractFact, ContractKey, HttpMethod, MethodSpec, ProviderFact, ServiceName,
};
use crate::federation::repo_id::GlobalId;
use crate::schema::GraphNode;

/// The per-service endpoint table the joiner builds and the
/// resolvers consult. Keyed by `(service, ContractKey)` so a
/// resolver can ask "does service X expose ContractKey Y?".
pub(crate) type EndpointTable = BTreeMap<(ServiceName, ContractKey), Vec<EndpointProviderRecord>>;

/// One provider record contributing to an endpoint. A single
/// `(service, ContractKey)` may carry several records when the
/// same route is emitted by both code and OpenAPI (merged into
/// one `Endpoint` later in step 2 of `ContractJoiner::run`).
#[derive(Debug, Clone)]
pub struct EndpointProviderRecord {
    pub id: GlobalId,
    pub fact: Option<ContractFact>,
    pub template: String,
    pub method: HttpMethod,
}

/// Build the per-service endpoint table from the projected
/// contract nodes. See module docs for the §7.2 grouping rules.
pub(crate) fn build_endpoints(
    nodes: &[GraphNode],
    assignments: &BTreeMap<String, ServiceName>,
    config: &ContractFederationConfig,
) -> EndpointTable {
    let mut table: EndpointTable = BTreeMap::new();
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

/// Compact human-readable sort key for an `EndpointId`. Used to
/// keep `ServiceInfo.endpoint_ids` sorted by the same
/// lexicographic order the §4.4 grammar implies.
pub(crate) fn endpoint_id_string(id: &EndpointId) -> String {
    id.0.to_string() + "|" + &id.1.to_string()
}

/// First/only route owner whose `(service, ContractKey)` passes
/// `filter`. Pass #4 R22 (review §D19) collapses the three
/// "enumerate endpoints matching a filter, return the
/// first/only route owner" sites that lived in the pre-refactor
/// `joiner.rs`:
/// - the `/graphql` route resolution in
///   [`resolve_graphql_consumer`](super::consumer_protocol::resolve_graphql_consumer);
/// - the env-host resolution in
///   [`target_service_from_hosts`](crate::federation::contracts::url_resolution::target_service_from_hosts);
/// - the rule-4 host-pattern resolution that lived at
///   `joiner.rs:300-312` (pre-split) and now flows through the
///   same helper.
///
/// Returns the first match's `ServiceName`. Callers that need
/// "exactly one" semantics (the `/graphql` route owner case)
/// should compare the count themselves — the helper exists to
/// dedupe the iteration, not to decide the cardinality.
pub(crate) fn route_owner<F>(endpoints: &EndpointTable, filter: F) -> Option<ServiceName>
where
    F: Fn(&(ServiceName, ContractKey)) -> bool,
{
    endpoints
        .keys()
        .find(|k| filter(k))
        .map(|(svc, _)| svc.clone())
}
