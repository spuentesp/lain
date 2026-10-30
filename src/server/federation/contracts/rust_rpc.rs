//! Source-rebuilt Rust RPC identity. Catalog matching crosses repository boundaries.
//! Evidence lives only in the derived ContractIndex; no new serialized graph type.
use super::index::ContractIndex;
use super::model::{ContractFact, ContractKey, HostPart, RpcConsumerFact, RpcSystem};
use crate::error::LainError;
use crate::federation::graph_backend::GraphBackend;
use crate::federation::repo_id::GlobalId;
use crate::schema::GraphNode;
use crate::server::sensors::grpc_consumer_sensor::{
    detect_rust_grpc_candidates, rust_candidate_name, RustGrpcCandidate, RustGrpcConfig,
    RUST_RPC_PREFIX,
};
use crate::server::sensors::util::rust_proto_snake_case;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustRpcDetail {
    pub status: String,
    pub candidate: Option<RustGrpcCandidate>,
    pub contract_key: Option<String>,
    pub compatible_keys: Vec<String>,
    pub enclosing_symbol: Option<String>,
    pub error: Option<String>,
    pub provider_ids: Vec<String>,
    pub identity_rule: Option<String>,
}

/// Re-read only files that indexing marked as Rust candidate sites. The reader
/// supplies live files or exact snapshot blobs (including pattern overrides).
/// A missing source/config is an explicit gap, never an old identity reused.
pub fn resolve(
    backend: &dyn GraphBackend,
    mut read: impl FnMut(&str, &str) -> Result<Option<String>, LainError>,
) -> Result<(Vec<GraphNode>, BTreeMap<GlobalId, RustRpcDetail>), LainError> {
    let mut markers = Vec::new();
    let mut catalog = BTreeSet::new();
    let mut provider_ids = BTreeMap::<(String, String), Vec<String>>::new();
    backend.visit_nodes(&mut |node| {
        if node.name.starts_with(RUST_RPC_PREFIX) {
            markers.push(node.clone());
        }
        for fact in &node.contract {
            if let ContractFact::RpcProvider(p) = fact {
                if p.system == RpcSystem::Grpc {
                    catalog.insert((p.service.clone(), p.method.clone()));
                    provider_ids
                        .entry((p.service.clone(), p.method.clone()))
                        .or_default()
                        .push(node.id.clone());
                }
            }
        }
    })?;
    let mut files = BTreeMap::new();
    for marker in &markers {
        let id = GlobalId::from_canonical(&marker.id);
        let key = (id.repo_id().to_string(), marker.path.clone());
        if files.contains_key(&key) {
            continue;
        }
        let result = (|| {
            let yaml = read(&key.0, ".lain/patterns/rust-grpc.yaml")?;
            let query = read(&key.0, ".lain/patterns/rust/grpc-candidate.scm")?;
            let config = RustGrpcConfig::from_sources(yaml.as_deref(), query.as_deref())?;
            let source = read(&key.0, &key.1)?.ok_or_else(|| {
                LainError::NotFound(format!("Rust RPC source {}:{}", key.0, key.1))
            })?;
            let candidates = detect_rust_grpc_candidates(&source, &key.1, &config)?;
            Ok::<_, LainError>((config, candidates))
        })()
        .map_err(|e| e.to_string());
        files.insert(key, result);
    }
    let mut details = BTreeMap::new();
    for node in &mut markers {
        let id = GlobalId::from_canonical(&node.id);
        // Clearing first retracts a previously derived identity if a provider,
        // source blob or mapping has disappeared since the last rejoin.
        node.contract.clear();
        let mut detail = RustRpcDetail {
            status: "missing_evidence".into(),
            candidate: None,
            contract_key: None,
            compatible_keys: vec![],
            enclosing_symbol: None,
            error: None,
            provider_ids: vec![],
            identity_rule: None,
        };
        match &files[&(id.repo_id().to_string(), node.path.clone())] {
            Err(error) => detail.error = Some(error.clone()),
            Ok((config, candidates)) => {
                if let Some(candidate) = candidates
                    .iter()
                    .find(|c| rust_candidate_name(c) == node.name)
                {
                    let keys = catalog_matches(candidate, config, &catalog);
                    detail.status = match keys.len() {
                        0 => "unknown_identity",
                        1 => "resolved",
                        _ => "ambiguous_identity",
                    }
                    .into();
                    detail.candidate = Some(candidate.clone());
                    detail.compatible_keys = keys.iter().map(ToString::to_string).collect();
                    detail.identity_rule = Some(format!(
                        "catalog module={:?}, type={:?}, protobuf Rust snake_case method={}",
                        config.client_module_template,
                        config.client_type_template,
                        candidate.method
                    ));
                    for key in &keys {
                        if let ContractKey::Rpc {
                            service, method, ..
                        } = key
                        {
                            detail.provider_ids.extend(
                                provider_ids
                                    .get(&(service.clone(), method.clone()))
                                    .cloned()
                                    .unwrap_or_default(),
                            );
                        }
                    }
                    detail.provider_ids.sort();
                    detail.provider_ids.dedup();
                    if let [ContractKey::Rpc {
                        system,
                        service,
                        method,
                    }] = keys.as_slice()
                    {
                        detail.contract_key = Some(keys[0].to_string());
                        node.contract
                            .push(ContractFact::RpcConsumer(RpcConsumerFact {
                                system: *system,
                                service: service.clone(),
                                method: method.clone(),
                                channel_target: None,
                                channel_host_part: HostPart::None,
                            }));
                    }
                } else {
                    detail.error = Some(
                        "indexed site no longer has generated-client evidence; reindex required"
                            .into(),
                    );
                }
            }
        }
        details.insert(id, detail);
    }
    // Calls anchors are materialized by the sensor before projection. This is
    // the actual enclosing symbol, not an impl-name service guess.
    for edge in backend.all_edges()? {
        if edge.edge_type == crate::schema::EdgeType::Calls {
            if let Some(detail) = details.get_mut(&GlobalId::from_canonical(&edge.target_id)) {
                detail.enclosing_symbol = Some(edge.source_id);
            }
        }
    }
    backend.upsert_nodes_batch(&markers)?;
    Ok((markers, details))
}

fn catalog_matches(
    candidate: &RustGrpcCandidate,
    config: &RustGrpcConfig,
    catalog: &BTreeSet<(String, String)>,
) -> Vec<ContractKey> {
    let (Some(module_template), Some(type_template)) =
        (&config.client_module_template, &config.client_type_template)
    else {
        return vec![];
    };
    let Some((module, ty)) = candidate.generated_type.rsplit_once("::") else {
        return vec![];
    };
    let module = module.rsplit("::").next().unwrap_or(module);
    catalog
        .iter()
        .filter_map(|(service, method)| {
            let bare = service.rsplit('.').next()?;
            let snake = rust_proto_snake_case(bare);
            let camel = snake
                .split('_')
                .map(|w| {
                    let mut c = w.chars();
                    c.next()
                        .map(|h| h.to_ascii_uppercase().to_string() + c.as_str())
                        .unwrap_or_default()
                })
                .collect::<String>();
            let expand = |template: &str| {
                template
                    .replace("{service_snake}", &snake)
                    .replace("{service_camel}", &camel)
                    .replace("{service}", bare)
            };
            if module == expand(module_template)
                && ty == expand(type_template)
                && candidate.method.trim_start_matches("r#") == rust_proto_snake_case(method)
            {
                Some(ContractKey::Rpc {
                    system: RpcSystem::Grpc,
                    service: service.clone(),
                    method: method.clone(),
                })
            } else {
                None
            }
        })
        .collect()
}

/// Unknown identity has no RpcConsumerFact. It still downgrades coverage and
/// appears in list_unresolved instead of vanishing from the impact boundary.
pub fn attach(index: &mut ContractIndex, details: BTreeMap<GlobalId, RustRpcDetail>) {
    for (id, detail) in &details {
        if (detail.contract_key.is_none() || detail.enclosing_symbol.is_none())
            && !index.unnormalized.contains(id)
        {
            index.unnormalized.push(id.clone());
        }
    }
    index.unnormalized.sort();
    index.rust_rpcs = details;
}
