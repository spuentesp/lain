//! `apply_contract_context`: declare providers the analyzer cannot see, into
//! the federation's `repos.yaml`, without a restart.
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
use crate::federation::contracts::config::{ContractFederationConfig, ProviderDecl};
use crate::federation::contracts::context::{merge_provides, restore, write_guarded};
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

    // Validate the whole result before anything is written. The candidate is
    // the live contract config plus the new declarations.
    let current: ContractFederationConfig = fed
        .contract_config()
        .map(|c| (*c).clone())
        .unwrap_or_default();
    let mut candidate = current.clone();
    for decl in &incoming {
        if !candidate
            .provides
            .iter()
            .any(|p| p.service == decl.service && p.key == decl.key)
        {
            candidate.provides.push(decl.clone());
        }
    }
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

    let added = merge_provides(&mut doc, &incoming)
        .map_err(|e| fail("invalid_config", e.to_string(), json!({})))?;
    let new_yaml = serde_yaml::to_string(&doc)
        .map_err(|e| fail("internal", format!("serialize repos.yaml: {e}"), json!({})))?;

    let data = |applied: bool, extra: Value| {
        let mut base = json!({
            "applied": applied,
            "dry_run": dry_run,
            "config_path": config_path.display().to_string(),
            "added": added.iter().map(|d| json!({"service": d.service, "key": d.key})).collect::<Vec<_>>(),
        });
        if let (Some(b), Value::Object(e)) = (base.as_object_mut(), extra) {
            b.extend(e);
        }
        base
    };

    if dry_run || added.is_empty() {
        let note = if added.is_empty() {
            "every declaration is already present; nothing to write"
        } else {
            "dry run: nothing written; pass dry_run=false to apply"
        };
        let body = data(false, json!({"yaml": new_yaml, "note": note}));
        return Ok(respond(body, started, note));
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
    let served = fed.contract_index().is_some_and(|index| {
        index.endpoints.values().any(|e| {
            e.providers
                .iter()
                .any(|p| p.node_id.as_str().contains("repos.yaml#provides["))
        })
    });
    if rejoin.is_err() || !served {
        // Roll the file and the live config back. Nothing claims success.
        let _ = restore(&config_path, &original);
        fed.set_contract_config(current);
        let _ = fed.rejoin_contracts_if_dirty();
        let reason = match &rejoin {
            Err(e) => format!("re-join failed: {e}"),
            Ok(()) => "the declaration is not served after re-join".to_string(),
        };
        return Err(fail(
            "rejoin_failed",
            format!("{reason}; repos.yaml was restored"),
            json!({"config_path": config_path.display().to_string()}),
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

fn respond(data: Value, started: Instant, text: &str) -> ToolOutcome {
    let envelope = success_envelope(data.clone(), "live", false, started);
    outcome(envelope, &data, text.to_string())
}
