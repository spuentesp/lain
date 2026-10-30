//! `apply_contract_context`: declare the contract context the analyzer
//! cannot see, into the federation's `repos.yaml`, without a restart.
//!
//! Three kinds of declaration are accepted:
//! - `provides` — an endpoint the service exposes (a route registered from
//!   a runtime config file, for instance).
//! - `host_aliases` — which service a host expression dials, for a consumer
//!   whose URL the analyzer cannot follow (a transparent reverse proxy).
//!
//! - `bindings` — an explicit mapping from an existing consumer selector to
//!   an existing compatible endpoint. Ambiguous selectors are refused.
//!
//! Dry-run by default: the tool returns the YAML it would write and touches
//! nothing. With `dry_run: false` it validates the whole result, writes
//! `repos.yaml` atomically (with a `.bak`), re-joins the contract index, and
//! reports success only if the declaration is served. A failed re-join rolls
//! the file back.

use std::time::Instant;

use serde_json::{json, Map, Value};

use super::envelope::{error_outcome, outcome, success_envelope};
use super::view::snapshot_label;
use super::{ContractToolEntry, ContractToolFuture, ToolOutcome};
use crate::federation::contracts::config::{
    ConfirmedBinding, ContractFederationConfig, HostAliasDecl, ProviderDecl,
    DECLARED_PROVIDER_PATH_PREFIX,
};
use crate::federation::contracts::context::{
    acquire_transaction, merge_bindings, merge_host_aliases, merge_provides, parse_binding_key,
};
use crate::federation::contracts::index::{ConsumerResolution, ConsumerTarget, ContractIndex};
use crate::federation::contracts::key_compat::keys_compatible;
use crate::federation::contracts::model::{ContractKey, ServiceName};
use crate::federation::federated_index::FederatedIndex;
use crate::federation::repo_id::GlobalId;
use crate::schema::EdgeProvenance;
use crate::server::mcp::handler::McpContext;

inventory::submit!(ContractToolEntry {
    name: "apply_contract_context",
    handler: apply_contract_context_handle,
});

/// `apply_contract_context` async handler.
pub fn apply_contract_context_handle<'a>(
    ctx: &'a McpContext<'a>,
    args: Value,
) -> ContractToolFuture<'a> {
    let args_map: Map<String, Value> = args.as_object().cloned().unwrap_or_default();
    let started = Instant::now();
    Box::pin(async move {
        match run_apply(ctx, args_map, started) {
            Ok(o) | Err(o) => Ok(o),
        }
    })
}

fn run_apply(
    ctx: &McpContext<'_>,
    args: Map<String, Value>,
    started: Instant,
) -> Result<ToolOutcome, ToolOutcome> {
    let snap = snapshot_label(&args);
    let fail = |code: &str, message: String, details: Value| {
        error_outcome(code, message, Some(details), &snap, started)
    };

    // Only the live federation has a `repos.yaml` to write.
    if args.get("snapshot").and_then(Value::as_str) != Some("live") {
        return Err(fail(
            "invalid_argument",
            "apply_contract_context writes the live federation: pass snapshot \"live\"".into(),
            json!({"arg": "snapshot"}),
        ));
    }
    let fed = ctx
        .federation
        .ok_or_else(|| fail("unavailable", "no federation is loaded".into(), json!({})))?;
    let config_path = fed.config_path().ok_or_else(|| {
        fail(
            "no_config_file",
            "this federation was not loaded from a repos.yaml; nothing to write".into(),
            json!({}),
        )
    })?;

    let incoming: Vec<ProviderDecl> = match args.get("provides") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            fail(
                "invalid_argument",
                format!("provides does not parse: {e}"),
                json!({"arg": "provides"}),
            )
        })?,
        None => Vec::new(),
    };
    let incoming_aliases: Vec<HostAliasDecl> = match args.get("host_aliases") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            fail(
                "invalid_argument",
                format!("host_aliases does not parse: {e}"),
                json!({"arg": "host_aliases"}),
            )
        })?,
        None => Vec::new(),
    };
    let incoming_bindings: Vec<ConfirmedBinding> = match args.get("bindings") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            fail(
                "invalid_argument",
                format!("bindings does not parse: {e}"),
                json!({"arg": "bindings"}),
            )
        })?,
        None => Vec::new(),
    };
    let dry_run = match args.get("dry_run") {
        Some(Value::Bool(value)) => *value,
        None => true,
        _ => {
            return Err(fail(
                "invalid_argument",
                "dry_run must be a boolean".into(),
                json!({"arg": "dry_run"}),
            ))
        }
    };
    // Hold the cooperating-writer lock across read, write, publication and recovery.
    // A preview does not even create a lock file.
    let transaction = if dry_run {
        None
    } else {
        Some(
            acquire_transaction(&config_path)
                .map_err(|e| fail("conflict", e.to_string(), json!({})))?,
        )
    };
    fed.rejoin_contracts_if_dirty()
        .map_err(|e| fail("rejoin_failed", e.to_string(), json!({})))?;
    validate_bindings(fed, &incoming_bindings)
        .map_err(|(code, message, details)| fail(code, message, details))?;

    let original = std::fs::read(&config_path).map_err(|e| {
        fail(
            "io_error",
            format!("cannot read {}: {e}", config_path.display()),
            json!({}),
        )
    })?;
    let mut doc: serde_yaml::Value = serde_yaml::from_slice(&original).map_err(|e| {
        fail(
            "invalid_config",
            format!("{} is not valid YAML: {e}", config_path.display()),
            json!({}),
        )
    })?;

    // The live contract config, kept only for the rollback.
    let current: ContractFederationConfig = fed
        .contract_config()
        .map(|c| (*c).clone())
        .unwrap_or_default();

    let added = merge_provides(&mut doc, &incoming)
        .map_err(|e| fail("invalid_config", e.to_string(), json!({})))?;
    let added_aliases = merge_host_aliases(&mut doc, &incoming_aliases)
        .map_err(|e| fail("invalid_config", e.to_string(), json!({})))?;
    let added_bindings = merge_bindings(&mut doc, &incoming_bindings)
        .map_err(|e| fail("conflict", e.to_string(), json!({"arg": "bindings"})))?;
    let new_yaml = serde_yaml::to_string(&doc)
        .map_err(|e| fail("internal", format!("serialize repos.yaml: {e}"), json!({})))?;

    // Validate the configuration that is actually written — the merged
    // document — not the live one. An operator may have edited the file
    // since the last reload, and validating a different configuration
    // from the one written would let an invalid declaration stand on
    // disk while the tool reported success.
    let candidate = ContractFederationConfig::load_from_str(&new_yaml).map_err(|e| {
        fail(
            "invalid_config",
            format!("the merged configuration does not parse: {e}"),
            json!({"detail": e.to_string()}),
        )
    })?;
    let repo_ids: Vec<String> = fed
        .list_repos()
        .into_iter()
        .map(|(id, _)| id.as_str().to_string())
        .collect();
    candidate.validate(&repo_ids).map_err(|e| {
        fail(
            "invalid_argument",
            format!("declaration refused: {e}"),
            json!({"detail": e.to_string()}),
        )
    })?;

    let data = |applied: bool, extra: Value| {
        let mut base = json!({
            "applied": applied,
            "dry_run": dry_run,
            "config_path": config_path.display().to_string(),
            "added": added.iter().map(|d| json!({"service": d.service, "key": d.key})).collect::<Vec<_>>(),
            "added_bindings": added_bindings,
            "added_host_aliases": added_aliases.iter().map(|a| json!({
                "host": a.host,
                "file": a.file,
                "service": a.service,
            })).collect::<Vec<_>>(),
        });
        if let (Some(b), Value::Object(e)) = (base.as_object_mut(), extra) {
            b.extend(e);
        }
        base
    };

    if dry_run {
        let note = "dry run: nothing written; pass dry_run=false to apply";
        let body = data(false, json!({"yaml": new_yaml, "note": note}));
        return Ok(respond(body, started, note));
    }
    if added.is_empty() && added_aliases.is_empty() && added_bindings.is_empty() {
        // Nothing to write — the declarations are already in the file.
        // `applied` says whether the requested declaration is in effect,
        // so report what the live index actually serves rather than the
        // fact that this call wrote nothing. A request that names no
        // declaration has nothing to be in effect.
        let requested =
            !incoming.is_empty() || !incoming_aliases.is_empty() || !incoming_bindings.is_empty();
        let served =
            requested && declaration_served(fed, &incoming, &incoming_aliases, &incoming_bindings);
        if !requested {
            let note = "no declaration in the request; nothing to write";
            let body = data(false, json!({"yaml": new_yaml, "note": note}));
            return Ok(respond(body, started, note));
        }
        if served {
            transaction
                .as_ref()
                .expect("live transaction")
                .verify(&original)
                .map_err(|e| fail("conflict", e.to_string(), json!({})))?;
            let note = "every declaration is already present and served; nothing to write";
            let body = data(true, json!({"yaml": new_yaml, "note": note}));
            return Ok(respond(body, started, note));
        }
        // The declaration is in the file but the live index does not
        // serve it: the watcher has not reloaded it, or the reload
        // failed. Fall through to the write and the re-join — the bytes
        // on disk do not change, and the re-join is what makes the
        // declaration live. Returning here would leave the operator
        // with a declaration the federation never serves, and no way
        // to make it serve from this tool.
    }

    let transaction = transaction.as_ref().expect("live write transaction");
    transaction
        .write_guarded(&original, new_yaml.as_bytes())
        .map_err(|e| {
            fail(
                "conflict",
                e.to_string(),
                json!({"config_path": config_path.display().to_string()}),
            )
        })?;

    // Apply in-process now, rather than waiting for the file watcher.
    fed.set_contract_config(candidate);
    let rejoin = fed.rejoin_contracts_if_dirty();
    let served = validate_bindings(fed, &incoming_bindings).is_ok()
        && declaration_served(fed, &incoming, &incoming_aliases, &incoming_bindings);
    let unchanged = transaction.verify(new_yaml.as_bytes());
    if rejoin.is_err() || !served || unchanged.is_err() {
        // Roll the file and the live config back. Nothing claims success.
        let restored = transaction.restore(new_yaml.as_bytes(), &original);
        // If an external writer won, recover from its current file rather than
        // publish our stale in-memory config over that writer's declaration.
        let recovery_config = if restored.is_ok() {
            Some(current)
        } else {
            std::fs::read_to_string(&config_path)
                .ok()
                .and_then(|yaml| ContractFederationConfig::load_from_str(&yaml).ok())
                .filter(|config| config.validate(&repo_ids).is_ok())
        };
        let recovery = recovery_config.map(|config| {
            fed.set_contract_config(config);
            fed.rejoin_contracts_if_dirty()
        });
        let reason = match &rejoin {
            Err(e) => format!("re-join failed: {e}"),
            Ok(()) => unchanged
                .as_ref()
                .err()
                .map(ToString::to_string)
                .unwrap_or_else(|| "the declaration is not served after re-join".to_string()),
        };
        // Report failed recovery explicitly; external content may now differ
        // from both the original and our candidate configuration.
        let restored_note = match &restored {
            Ok(()) => "repos.yaml was restored".to_string(),
            Err(e) => {
                format!(
                    "repos.yaml could NOT be restored ({e}); current disk content was left alone"
                )
            }
        };
        return Err(fail(
            "rejoin_failed",
            format!("{reason}; {restored_note}"),
            json!({
                "config_path": config_path.display().to_string(),
                "restored": restored.is_ok(),
                "recovered": matches!(recovery, Some(Ok(()))),
            }),
        ));
    }

    let body = data(
        true,
        json!({"backup": crate::federation::contracts::context::backup_path(&config_path).display().to_string()}),
    );
    Ok(respond(
        body,
        started,
        "declaration applied and served by the re-joined contract index",
    ))
}

/// Whether every declared provider in `incoming` is served by `index`.
///
/// A declaration is served when the index carries an endpoint for its
/// service whose provider record is the declaration itself — the node
/// id names the `repos.yaml#provides[i]` section it came from, and its
/// name segment is the declared key. Matching on the key rather than
/// on the endpoint's template keeps the check exact for every key kind
/// (a `topic:` declaration has no path template to compare).
///
/// Every declaration must be served: a request that declares two
/// providers and lands one is a partial application.
fn declared_providers_served(
    index: &crate::federation::contracts::index::ContractIndex,
    incoming: &[ProviderDecl],
) -> bool {
    incoming.iter().all(|d| {
        index.endpoints.values().any(|e| {
            e.id.0 .0 == d.service
                && e.providers.iter().any(|p| {
                    p.node_id
                        .path()
                        .is_some_and(|path| path.starts_with(DECLARED_PROVIDER_PATH_PREFIX))
                        && p.node_id.name().as_deref() == Some(d.key.as_str())
                })
        })
    })
}

/// Whether the live contract index serves the declarations.
///
/// The declaration must be *served* before the tool claims success: a
/// declared provider must show up in the endpoint table, a declared host
/// alias must be live in the contract config the joiner just ran
/// against. Either way the contract index must exist.
///
/// *Every* declaration in the request must be served. A request that
/// declares two providers and lands one is a partial application, and
/// reporting it as applied would tell the operator that a provider the
/// index does not serve is live.
fn declaration_served(
    fed: &FederatedIndex,
    incoming: &[ProviderDecl],
    incoming_aliases: &[HostAliasDecl],
    incoming_bindings: &[ConfirmedBinding],
) -> bool {
    let index = fed.contract_index();
    let providers_served = index
        .as_ref()
        .is_some_and(|index| declared_providers_served(index, incoming));
    let aliases_served = incoming_aliases.iter().all(|d| {
        fed.contract_config().is_some_and(|c| {
            c.host_aliases
                .iter()
                .any(|a| a.host == d.host && a.file == d.file && a.service == d.service)
        })
    });
    index.is_some() && providers_served && aliases_served && bindings_served(fed, incoming_bindings)
}

/// Match the legacy selector exactly as the confirmed joiner does. The selector
/// carries no line/id, so a request must never imply per-site precision.
fn matching_consumers(
    backend: &dyn crate::federation::graph_backend::GraphBackend,
    index: &ContractIndex,
    binding: &ConfirmedBinding,
    key: &ContractKey,
) -> Result<Vec<(GlobalId, crate::schema::GraphNode)>, String> {
    let mut matches = Vec::new();
    for id in index
        .consumers
        .keys()
        .filter(|id| id.repo_id() == binding.consumer.repo)
    {
        let node = backend.get_node(id.as_str()).map_err(|e| e.to_string())?;
        let Some(node) = node else { continue };
        if node.path == binding.consumer.path
            && node.container.as_ref().unwrap_or(&node.name) == &binding.consumer.symbol
            && node
                .contract
                .iter()
                .flat_map(|fact| fact.consumer_keys())
                .any(|candidate| &candidate == key)
        {
            matches.push((id.clone(), node));
        }
    }
    Ok(matches)
}

type BindingFailure = (&'static str, String, Value);

/// Revalidate the actual facts and published resolutions, never a prior check
/// result. Key compatibility is shared with check_binding and the joiner.
fn validate_bindings(
    fed: &FederatedIndex,
    bindings: &[ConfirmedBinding],
) -> Result<(), BindingFailure> {
    if bindings.is_empty() {
        return Ok(());
    }
    fed.with_contract_snapshot(|snapshot, backend| {
        let index = &snapshot.index;
        for binding in bindings {
            let refuse = |reason: &str| {
                (
                    "invalid_binding",
                    reason.to_string(),
                    json!({"reason": reason, "binding": binding}),
                )
            };
            let consumer_key: ContractKey = parse_binding_key(&binding.consumer.key)
                .map_err(|_| refuse("malformed_consumer_key"))?;
            let provider_key: ContractKey = parse_binding_key(&binding.provider.key)
                .map_err(|_| refuse("malformed_provider_key"))?;
            if !keys_compatible(&consumer_key, &provider_key) {
                return Err(refuse("incompatible_keys"));
            }
            let endpoint = (ServiceName(binding.provider.service.clone()), provider_key);
            let provider = index
                .endpoints
                .get(&endpoint)
                .ok_or_else(|| refuse("missing_provider"))?;
            if provider.providers.is_empty()
                || provider.providers.iter().any(|p| {
                    !is_declared_provider(&p.node_id)
                        && backend
                            .get_node(p.node_id.as_str())
                            .ok()
                            .flatten()
                            .is_none()
                })
            {
                return Err(refuse("missing_provider"));
            }
            let matches = matching_consumers(backend, index, binding, &consumer_key)
                .map_err(|e| ("unavailable", e, json!({})))?;
            if matches.is_empty() {
                return Err(refuse("missing_consumer"));
            }
            if matches.len() != 1 {
                return Err((
                    "ambiguous_binding",
                    "consumer selector matches multiple sites; repos.yaml bindings cannot select one site".into(),
                    json!({
                        "binding": binding,
                        "matched_consumers": matches.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
                    }),
                ));
            }
            let (consumer_id, consumer_node) = &matches[0];
            let matching_facts = consumer_node
                .contract
                .iter()
                .filter(|fact| fact.consumer_keys().iter().any(|key| key == &consumer_key))
                .count();
            if matching_facts != 1 {
                return Err((
                    "ambiguous_binding",
                    "consumer selector and key match multiple facts on one site".into(),
                    json!({"binding": binding, "matching_facts": matching_facts}),
                ));
            }
            let resolutions = index
                .consumers
                .get(consumer_id)
                .ok_or_else(|| refuse("missing_consumer"))?;
            if resolutions.is_empty() {
                return Err(refuse("missing_consumer"));
            }
            // Service ownership is shared by all resolutions for a call site;
            // check the whole set while target decisions stay key-specific.
            if resolutions
                .iter()
                .any(|resolution| resolution.service == endpoint.0)
            {
                return Err(refuse("same_service"));
            }
            // A node can carry several independent facts and therefore several
            // resolutions. Only resolutions that bind a key compatible with
            // this fact's key can represent its current destination. A
            // different topic on the same node must not reject this mapping.
            let selected: Vec<&ConsumerResolution> = resolutions
                .iter()
                .filter(|resolution| {
                    resolution.bound_endpoints.iter().any(|(_, key)| {
                        keys_compatible(&consumer_key, key)
                    })
                })
                .collect();
            if selected.iter().any(|resolution| {
                matches!(resolution.target, Some(ConsumerTarget::Binds { .. }))
                    && !resolution.bound_endpoints.contains(&endpoint)
            }) {
                return Err((
                    "conflict",
                    "consumer is already bound to a different endpoint".into(),
                    json!({"binding": binding, "reason": "already_bound"}),
                ));
            }
        }
        Ok(())
    })
    .unwrap_or_else(|| Err(("unavailable", "no current contract index".into(), json!({}))))
}

fn is_declared_provider(id: &GlobalId) -> bool {
    id.path()
        .is_some_and(|path| path.starts_with(DECLARED_PROVIDER_PATH_PREFIX))
}

fn bindings_served(fed: &FederatedIndex, bindings: &[ConfirmedBinding]) -> bool {
    if bindings.is_empty() {
        return true;
    }
    fed.with_contract_snapshot(|snapshot, backend| {
        let index = &snapshot.index;
        bindings.iter().all(|binding| {
            let Ok(key) = parse_binding_key(&binding.consumer.key) else {
                return false;
            };
            let Ok(provider_key) = parse_binding_key(&binding.provider.key) else {
                return false;
            };
            let endpoint = (ServiceName(binding.provider.service.clone()), provider_key);
            let Ok(matches) = matching_consumers(backend, index, binding, &key) else {
                return false;
            };
            if matches.len() != 1 {
                return false;
            }
            let (consumer, _) = &matches[0];
            index.consumers.get(consumer).is_some_and(|resolutions| {
                resolutions.iter().any(|r| {
                    matches!(
                        r.target,
                        Some(ConsumerTarget::Binds {
                            provenance: EdgeProvenance::Confirmed { .. },
                            ..
                        })
                    ) && r.bound_endpoints.contains(&endpoint)
                })
            }) && index.endpoints.get(&endpoint).is_some_and(|definition| {
                definition.providers.iter().any(|provider| {
                    is_declared_provider(&provider.node_id)
                        || snapshot.binds.iter().any(|(source, target)| {
                            source == consumer.as_str() && target == provider.node_id.as_str()
                        })
                })
            })
        })
    })
    .unwrap_or(false)
}

fn respond(data: Value, started: Instant, text: &str) -> ToolOutcome {
    let envelope = success_envelope(data.clone(), "live", false, started);
    outcome(envelope, &data, text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::contracts::index::{ContractIndex, Endpoint, EndpointProvider};
    use crate::federation::contracts::model::{ContractKey, Direction, HttpMethod, ServiceName};
    use crate::federation::repo_id::RepoId;
    use crate::schema::NodeType;
    use std::collections::BTreeMap;

    fn decl(service: &str, key: &str) -> ProviderDecl {
        ProviderDecl {
            service: service.into(),
            key: key.into(),
            source: None,
        }
    }

    /// An endpoint for `service` whose only provider is the declared
    /// provider at `repos.yaml#provides[i]`, with `key` as its declared
    /// key — the shape the joiner mints.
    fn declared_endpoint(service: &str, key: &str, i: u32, template: &str) -> Endpoint {
        let repo = RepoId::new("nat").expect("repo id");
        let node_id = crate::federation::repo_id::GlobalId::new(
            &repo,
            NodeType::HttpRoute,
            &format!("{DECLARED_PROVIDER_PATH_PREFIX}{i}]"),
            key,
            Some(i),
        );
        Endpoint {
            id: (
                ServiceName(service.into()),
                ContractKey::Http {
                    method: crate::federation::contracts::model::MethodSpec::Known(
                        HttpMethod::Post,
                    ),
                    template: template.into(),
                },
            ),
            method: HttpMethod::Post,
            template: template.into(),
            providers: vec![EndpointProvider {
                node_id,
                origin: crate::federation::contracts::model::ProviderOrigin::Code,
                handler: None,
                operation_id: None,
            }],
            schemas: BTreeMap::<Direction, _>::new(),
        }
    }

    fn index_with(endpoints: Vec<Endpoint>) -> ContractIndex {
        let mut index = ContractIndex::default();
        for e in endpoints {
            index.endpoints.insert(e.id.clone(), e);
        }
        index
    }

    /// Both declarations are served: the request is fully applied.
    #[test]
    fn every_declared_provider_served_is_true() {
        let index = index_with(vec![
            declared_endpoint("nat", "http:POST /chat", 0, "/chat"),
            declared_endpoint("nat", "http:POST /stream", 1, "/stream"),
        ]);
        assert!(declared_providers_served(
            &index,
            &[
                decl("nat", "http:POST /chat"),
                decl("nat", "http:POST /stream")
            ]
        ));
    }

    /// Only one of the two declarations landed. The request is a
    /// partial application, and the tool must not report it as served.
    #[test]
    fn a_partially_served_declaration_is_not_served() {
        let index = index_with(vec![declared_endpoint(
            "nat",
            "http:POST /chat",
            0,
            "/chat",
        )]);
        assert!(!declared_providers_served(
            &index,
            &[
                decl("nat", "http:POST /chat"),
                decl("nat", "http:POST /stream")
            ]
        ));
    }

    /// A declaration of another service is not served by this one's
    /// endpoint, even when the key is the same.
    #[test]
    fn a_declaration_of_another_service_is_not_served() {
        let index = index_with(vec![declared_endpoint(
            "nat",
            "http:POST /chat",
            0,
            "/chat",
        )]);
        assert!(!declared_providers_served(
            &index,
            &[decl("other", "http:POST /chat")]
        ));
    }

    /// A key of another kind matches only its own declaration: the
    /// check compares the declared key, not the endpoint's template.
    #[test]
    fn a_declaration_matches_only_its_own_key() {
        let index = index_with(vec![declared_endpoint(
            "nat",
            "http:POST /chat",
            0,
            "/chat",
        )]);
        assert!(!declared_providers_served(
            &index,
            &[decl("nat", "topic:kafka/orders.created")]
        ));
    }
}
