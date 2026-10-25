//! `list_service_refs`: URL literals that name a declared service, per repo.
//!
//! Live view only. References are recorded when a repo is projected
//! (`FederatedIndex::refresh_service_refs`), so they describe the revision the
//! federation holds now. A reference is evidence of a target, not of a call;
//! `linked_to_call` says whether a sensor also detected a call at that line.

use super::contracts::{parse_cursor, parse_limit};
use super::envelope::{check_api_version, error_outcome, outcome, success_envelope_with_view};
use super::paging::{apply_limit, fingerprint};
use super::view::{resolve_view, snapshot_label};
use super::{ContractToolEntry, ContractToolFuture, ToolOutcome};
use crate::federation::contracts::index::ContractIndex;
use crate::federation::contracts::model::ServiceName;
use crate::federation::contracts::service_refs::{RefOutcome, ServiceRef, Usage};
use crate::federation::contracts::suggest::suggest_routes;
use crate::federation::federated_index::FederatedIndex;
use crate::server::mcp::handler::McpContext;
use serde_json::{json, Map, Value};
use std::sync::Arc;
use std::time::Instant;

inventory::submit!(ContractToolEntry {
    name: "list_service_refs",
    handler: list_service_refs_handle,
});

const USAGE_VALUES: &[&str] = &["production", "test", "configuration", "documentation"];
const OUTCOME_VALUES: &[&str] = &["resolved", "ambiguous", "unknown"];
/// Default score a suggestion must reach. Matches the on-disk measurement.
const DEFAULT_MIN_SCORE: f32 = 0.6;
/// Routes proposed per reference, best first.
const SUGGESTIONS_PER_REF: usize = 2;

fn object_or_empty(args: Value) -> Map<String, Value> {
    match args {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// `list_service_refs` async handler.
pub fn list_service_refs_handle<'a>(
    ctx: &'a McpContext<'a>,
    args: Value,
) -> ContractToolFuture<'a> {
    let args_map = object_or_empty(args);
    let started = Instant::now();
    Box::pin(async move {
        if let Err(details) = check_api_version(&args_map) {
            return Ok(error_outcome(
                "unsupported_api_version",
                "api_version not served",
                Some(details),
                &snapshot_label(&args_map),
                started,
            ));
        }
        match run_list_service_refs(ctx, args_map, started).await {
            Ok(o) => Ok(o),
            Err(o) => Ok(o),
        }
    })
}

fn usage_name(u: Usage) -> &'static str {
    match u {
        Usage::Production => "production",
        Usage::Test => "test",
        Usage::Configuration => "configuration",
        Usage::Documentation => "documentation",
    }
}

fn outcome_kind(o: &RefOutcome) -> &'static str {
    match o {
        RefOutcome::Resolved(_) => "resolved",
        RefOutcome::Ambiguous(_) => "ambiguous",
        RefOutcome::UnknownHost => "unknown",
    }
}

/// Suggested routes for the production references of the repos in scope.
/// These are proposals, never links: each carries `suggested: true` and a
/// score. `status` says why there are none when there are none: no server
/// context, no contract index, or no embedding model installed.
fn suggestions_for(
    ctx: &McpContext<'_>,
    federation: &FederatedIndex,
    index: Option<Arc<ContractIndex>>,
    repo_filter: Option<&str>,
    min_score: f32,
) -> (Vec<Value>, &'static str) {
    let Some(server) = ctx.server else {
        return (Vec::new(), "no_server_context");
    };
    let embedder = server.ingest().embedder();
    if embedder.is_stub() {
        return (Vec::new(), "no_embedding_model");
    }
    let Some(index) = index else {
        return (Vec::new(), "no_contract_index");
    };
    let routes: Vec<(ServiceName, String)> = index
        .endpoints
        .iter()
        .filter_map(|(id, _)| {
            let key = id.1.to_string();
            key.strip_prefix("http:")
                .map(|selector| (id.0.clone(), selector.to_string()))
        })
        .collect();
    let embed = |text: &str| embedder.embed(text).ok();
    let mut out = Vec::new();
    for (repo_id, _) in federation.list_repos() {
        if repo_filter.is_some_and(|r| repo_id.as_str() != r) {
            continue;
        }
        let refs = federation.service_refs(&repo_id);
        for s in suggest_routes(&refs, &routes, &embed, min_score, SUGGESTIONS_PER_REF) {
            out.push(json!({
                "repo": repo_id.as_str(),
                "path": s.path,
                "line": s.line,
                "url": s.url,
                "service": s.service.0,
                "route": s.route,
                "score": s.score,
                "suggested": true,
            }));
        }
    }
    (out, "ok")
}

fn ref_json(repo: &str, r: &ServiceRef) -> Value {
    let outcome = match &r.outcome {
        RefOutcome::Resolved(s) => json!({"kind": "resolved", "service": s.0}),
        RefOutcome::Ambiguous(all) => json!({
            "kind": "ambiguous",
            "services": all.iter().map(|s| s.0.clone()).collect::<Vec<_>>(),
        }),
        RefOutcome::UnknownHost => json!({"kind": "unknown"}),
    };
    json!({
        "repo": repo,
        "path": r.path,
        "line": r.line,
        "value": r.value,
        "host": r.host,
        "outcome": outcome,
        "usage": usage_name(r.usage),
        "self_reference": r.self_reference,
        "linked_to_call": r.linked_to_call,
    })
}

/// Paging key: stable across calls for the same repo, path, line and literal.
fn item_key(it: &Value) -> String {
    format!(
        "{}|{}|{:010}|{}",
        it["repo"].as_str().unwrap_or(""),
        it["path"].as_str().unwrap_or(""),
        it["line"].as_u64().unwrap_or(0),
        it["value"].as_str().unwrap_or("")
    )
}

async fn run_list_service_refs(
    ctx: &McpContext<'_>,
    args_map: Map<String, Value>,
    started: Instant,
) -> Result<ToolOutcome, ToolOutcome> {
    let label = snapshot_label(&args_map);
    if label != "live" {
        return Err(error_outcome(
            "invalid_argument",
            "list_service_refs serves the live view only",
            Some(json!({"arg": "snapshot", "reason": "not_live", "allowed": ["live"]})),
            &label,
            started,
        ));
    }
    let usage_filter = args_map
        .get("usage")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if let Some(u) = &usage_filter {
        if !USAGE_VALUES.contains(&u.as_str()) {
            return Err(error_outcome(
                "invalid_argument",
                format!("unknown usage {u:?}"),
                Some(json!({"arg": "usage", "reason": "unknown", "allowed": USAGE_VALUES})),
                &label,
                started,
            ));
        }
    }
    let outcome_filter = args_map
        .get("outcome")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if let Some(o) = &outcome_filter {
        if !OUTCOME_VALUES.contains(&o.as_str()) {
            return Err(error_outcome(
                "invalid_argument",
                format!("unknown outcome {o:?}"),
                Some(json!({"arg": "outcome", "reason": "unknown", "allowed": OUTCOME_VALUES})),
                &label,
                started,
            ));
        }
    }
    let repo_filter = args_map
        .get("repo")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let want_suggestions = args_map
        .get("suggestions")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let min_score = match args_map.get("min_score").and_then(|v| v.as_f64()) {
        None => DEFAULT_MIN_SCORE,
        Some(x) if (0.0..=1.0).contains(&x) => x as f32,
        Some(x) => {
            return Err(error_outcome(
                "invalid_argument",
                format!("min_score {x} is outside [0, 1]"),
                Some(json!({"arg": "min_score", "reason": "out_of_range"})),
                &label,
                started,
            ))
        }
    };

    let resolved = resolve_view(ctx, &args_map, started).await?;
    let limit = parse_limit(&args_map, started)?;
    let cursor = parse_cursor(&args_map, started)?;
    let Some(federation) = ctx.federation else {
        return Err(error_outcome(
            "federation_disabled",
            "this server is not configured with a federation",
            None,
            &label,
            started,
        ));
    };

    let mut items: Vec<Value> = Vec::new();
    for (repo_id, _) in federation.list_repos() {
        if let Some(r) = &repo_filter {
            if repo_id.as_str() != r {
                continue;
            }
        }
        for r in federation.service_refs(&repo_id) {
            if let Some(u) = &usage_filter {
                if usage_name(r.usage) != u {
                    continue;
                }
            }
            if let Some(o) = &outcome_filter {
                if outcome_kind(&r.outcome) != o {
                    continue;
                }
            }
            items.push(ref_json(repo_id.as_str(), &r));
        }
    }
    items.sort_by(|a, b| item_key(a).cmp(&item_key(b)));
    if let Some(after) = cursor.as_ref().map(|c| c.after.clone()) {
        items.retain(|it| item_key(it) > after);
    }
    let fp = fingerprint(&args_map);
    let (page, next_cursor) = apply_limit(items, limit, item_key, &fp);

    let mut data = json!({"items": page});
    if let Some(tok) = next_cursor {
        data["cursor"] = json!(tok);
    }
    if want_suggestions {
        let (suggestions, status) = suggestions_for(
            ctx,
            federation,
            resolved.index().cloned(),
            repo_filter.as_deref(),
            min_score,
        );
        data["suggestions"] = json!(suggestions);
        data["suggestions_status"] = json!(status);
    }
    data["scope"] = resolved.scope().clone();
    let envelope = success_envelope_with_view(
        data.clone(),
        resolved.label(),
        resolved.reproducible(),
        resolved.view_info(),
        started,
    );
    let text = format!(
        "{} service references",
        data["items"].as_array().map_or(0, |a| a.len())
    );
    Ok(outcome(envelope, &data, text))
}
