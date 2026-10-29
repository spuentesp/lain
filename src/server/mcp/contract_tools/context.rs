//! `apply_contract_context`: declare the contract context the analyzer
//! cannot see, into the federation's `repos.yaml`, without a restart.
//!
//! Two kinds of declaration are accepted:
//! - `provides` — an endpoint the service exposes (a route registered from
//!   a runtime config file, for instance).
//! - `host_aliases` — which service a host expression dials, for a consumer
//!   whose URL the analyzer cannot follow (a transparent reverse proxy).
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
    ContractFederationConfig, HostAliasDecl, ProviderDecl, DECLARED_PROVIDER_PATH_PREFIX,
};
use crate::federation::contracts::context::{
    merge_host_aliases, merge_provides, restore, write_guarded,
};
use crate::federation::federated_index::FederatedIndex;
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
    let dry_run = args.get("dry_run").and_then(Value::as_bool).unwrap_or(true);

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
    if added.is_empty() && added_aliases.is_empty() {
        // Nothing to write — the declarations are already in the file.
        // `applied` says whether the requested declaration is in effect,
        // so report what the live index actually serves rather than the
        // fact that this call wrote nothing. A request that names no
        // declaration has nothing to be in effect.
        let requested = !incoming.is_empty() || !incoming_aliases.is_empty();
        let served = requested && declaration_served(fed, &incoming, &incoming_aliases);
        if !requested {
            let note = "no declaration in the request; nothing to write";
            let body = data(false, json!({"yaml": new_yaml, "note": note}));
            return Ok(respond(body, started, note));
        }
        if served {
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

    write_guarded(&config_path, &original, new_yaml.as_bytes()).map_err(|e| {
        fail(
            "conflict",
            e.to_string(),
            json!({"config_path": config_path.display().to_string()}),
        )
    })?;

    // Apply in-process now, rather than waiting for the file watcher.
    fed.set_contract_config(candidate);
    let rejoin = fed.rejoin_contracts_if_dirty();
    let served = declaration_served(fed, &incoming, &incoming_aliases);
    if rejoin.is_err() || !served {
        // Roll the file and the live config back. Nothing claims success.
        let restored = restore(&config_path, new_yaml.as_bytes(), &original);
        fed.set_contract_config(current);
        let _ = fed.rejoin_contracts_if_dirty();
        let reason = match &rejoin {
            Err(e) => format!("re-join failed: {e}"),
            Ok(()) => "the declaration is not served after re-join".to_string(),
        };
        // A failed restore must not be reported as a restore: the file on
        // disk still carries a declaration the federation does not serve.
        let restored_note = match &restored {
            Ok(()) => "repos.yaml was restored".to_string(),
            Err(e) => {
                format!("repos.yaml could NOT be restored ({e}); it still carries the declaration")
            }
        };
        return Err(fail(
            "rejoin_failed",
            format!("{reason}; {restored_note}"),
            json!({
                "config_path": config_path.display().to_string(),
                "restored": restored.is_ok(),
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
    index.is_some() && providers_served && aliases_served
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
