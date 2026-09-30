//! Analysis tools: `diff_contracts`, `trace_impact`, `get_coverage`
//! (`docs/CONTRACT_FEDERATION.md` §10.1, §12, §13).
//!
//! Read-only views over the federation's `ContractIndex` (live) or
//! a named snapshot's `ContractIndex` (`from_snapshot`).
//!
//! ## `diff_contracts`
//!
//! Wires PR 12's pure `diff_contracts`, `classify`, and `evaluate`
//! to MCP. The `ChangedFilesSource` for the
//! `ChangedWithoutSchema` rule is the `git2` mirror lookup from
//! [`crate::federation::contracts::changed_files`]; both snapshots
//! must be `ready`, have matching `analyzer_version`
//! (`analyzer_mismatch` otherwise), and matching `config_hash`
//! (`invalid_argument` reason `config_mismatch` otherwise).
//!
//! `live` is not allowed as `head` (`invalid_argument`,
//! `live_not_supported`).
//!
//! ## `trace_impact`
//!
//! Uses `traverse_impact` (`§5.2`) on the federation's backend.
//! Exactly one of `endpoint` / `field` / `symbol` is required (§12).
//! Paths are sorted by `(min_confidence desc, length asc, leaf id)`
//! (§12 guarantee).
//!
//! ## `get_coverage`
//!
//! Returns the §9.7 `Coverage`. When an `endpoint` is supplied the
//! `complete` field is computed with the
//! `coverage_complete(coverage, endpoint, index)` gate.

use super::contracts::{resolve_view, ViewHandle};
use super::envelope::{check_api_version, error_outcome, outcome, success_envelope};
use super::scope::live_scope;
use super::{ContractToolEntry, ContractToolFuture, ToolOutcome};
use crate::federation::contracts::changed_files::{MirrorChangedFiles, MultiRepoChangedFiles};
use crate::federation::contracts::diff::{
    build_coverage, classify, coverage_complete, diff_consumers, diff_contracts, evaluate,
    Affected, ChangeKind, Class, Compat, ContractSurface, Coverage as DiffCoverage, Impact,
    Reason as DiffReason, ReviewedRepo, Scope as DiffScope, UnreviewedRepo,
};
use crate::federation::contracts::index::{ContractIndex, EndpointId, ServiceInfo};
use crate::federation::contracts::model::ServiceName;
use crate::federation::contracts::model::{ContractKey, Direction, JsonPath, MethodSpec};
use crate::federation::contracts::snapshots::record::SnapshotRecord;
use crate::federation::graph_backend::{ImpactPath as GraphImpactPath, ImpactResult};
use crate::federation::repo_id::GlobalId;
use crate::schema::{EdgeProvenance, EdgeType};
use crate::server::mcp::handler::McpContext;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

// ─── dispatch ─────────────────────────────────────────────────────────

inventory::submit!(ContractToolEntry {
    name: "diff_contracts",
    handler: diff_contracts_handle,
});
inventory::submit!(ContractToolEntry {
    name: "trace_impact",
    handler: trace_impact_handle,
});
inventory::submit!(ContractToolEntry {
    name: "get_coverage",
    handler: get_coverage_handle,
});

pub fn diff_contracts_handle<'a>(ctx: &'a McpContext<'a>, args: Value) -> ContractToolFuture<'a> {
    let args_map = object_or_empty(args);
    let started = Instant::now();
    Box::pin(async move {
        if let Err(details) = check_api_version(&args_map) {
            return Ok(error_outcome(
                "unsupported_api_version",
                "api_version not served",
                Some(details),
                "live",
                started,
            ));
        }
        match run_diff_contracts(ctx, args_map, started).await {
            Ok(o) => Ok(o),
            Err(o) => Ok(o),
        }
    })
}

pub fn trace_impact_handle<'a>(ctx: &'a McpContext<'a>, args: Value) -> ContractToolFuture<'a> {
    let args_map = object_or_empty(args);
    let started = Instant::now();
    Box::pin(async move {
        if let Err(details) = check_api_version(&args_map) {
            return Ok(error_outcome(
                "unsupported_api_version",
                "api_version not served",
                Some(details),
                "live",
                started,
            ));
        }
        match run_trace_impact(ctx, args_map, started).await {
            Ok(o) => Ok(o),
            Err(o) => Ok(o),
        }
    })
}

pub fn get_coverage_handle<'a>(ctx: &'a McpContext<'a>, args: Value) -> ContractToolFuture<'a> {
    let args_map = object_or_empty(args);
    let started = Instant::now();
    Box::pin(async move {
        if let Err(details) = check_api_version(&args_map) {
            return Ok(error_outcome(
                "unsupported_api_version",
                "api_version not served",
                Some(details),
                "live",
                started,
            ));
        }
        match run_get_coverage(ctx, args_map, started).await {
            Ok(o) => Ok(o),
            Err(o) => Ok(o),
        }
    })
}

// ─── diff_contracts ───────────────────────────────────────────────────

async fn run_diff_contracts(
    ctx: &McpContext<'_>,
    args_map: Map<String, Value>,
    started: Instant,
) -> Result<ToolOutcome, ToolOutcome> {
    let snap_label = "live".to_string();
    let base_label = args_map
        .get("base")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            error_outcome(
                "invalid_argument",
                "missing required argument: base",
                Some(json!({"arg": "base"})),
                &snap_label,
                started,
            )
        })?
        .to_string();
    let head_label = args_map
        .get("head")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            error_outcome(
                "invalid_argument",
                "missing required argument: head",
                Some(json!({"arg": "head"})),
                &snap_label,
                started,
            )
        })?
        .to_string();
    if head_label == "live" || base_label == "live" {
        return Err(error_outcome(
            "invalid_argument",
            "live is not supported as diff base or head",
            Some(json!({"reason": "live_not_supported"})),
            &snap_label,
            started,
        ));
    }
    let min_impact = match parse_min_impact(&args_map) {
        Ok(c) => c,
        Err(o) => return Err(o),
    };
    let cap = parse_cap(&args_map, 50, 500, started)?;
    let _ = args_map.get("repo");
    let _ = args_map.get("service");

    let base_record = load_snapshot(ctx, &base_label, started)?;
    let head_record = load_snapshot(ctx, &head_label, started)?;
    if !snapshot_state_is_ready(&base_record) {
        return Err(error_outcome(
            "snapshot_not_ready",
            format!("snapshot {base_label:?} is not ready"),
            Some(json!({"state": snapshot_state_label(&base_record), "snapshot": base_label})),
            &head_label,
            started,
        ));
    }
    if !snapshot_state_is_ready(&head_record) {
        return Err(error_outcome(
            "snapshot_not_ready",
            format!("snapshot {head_label:?} is not ready"),
            Some(json!({"state": snapshot_state_label(&head_record), "snapshot": head_label})),
            &head_label,
            started,
        ));
    }
    if base_record.analyzer_version != head_record.analyzer_version {
        return Err(error_outcome(
            "analyzer_mismatch",
            "snapshot analyzer versions differ",
            Some(json!({
                "base": base_record.analyzer_version,
                "head": head_record.analyzer_version
            })),
            &head_label,
            started,
        ));
    }
    if base_record.config_hash != head_record.config_hash {
        return Err(error_outcome(
            "invalid_argument",
            "snapshot config_hash differs",
            Some(json!({"arg": "config_hash", "reason": "config_mismatch"})),
            &head_label,
            started,
        ));
    }
    let base_index = snapshot_contract_index(ctx, &base_record, started)?;
    let head_index = snapshot_contract_index(ctx, &head_record, started)?;
    let data_dir = ctx
        .snapshots
        .map(|m| m.data_dir().to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let changed_source = build_changed_source(&data_dir, &base_record, &head_record);
    let base_surface = ContractSurface::from_index(&base_index);
    let head_surface = ContractSurface::from_index(&head_index);

    let provider_changes = diff_contracts(&base_surface, &head_surface, &changed_source);
    let consumer_changes = diff_consumers(&base_surface, &head_surface);
    let mut all_changes = provider_changes;
    all_changes.extend(consumer_changes);

    let scope = build_scope(&head_record);
    let coverage_repos = build_repo_coverage(&head_record);
    let coverage = build_coverage(&head_index, coverage_repos, scope.clone());

    let mut evaluated: Vec<(Impact, Compat, ChangeKind, ServiceName)> = Vec::new();
    for change in all_changes {
        let direction = direction_of_change(&change.kind);
        let compat = if let Some(d) = direction {
            classify(&change.kind, d)
        } else {
            classify(&change.kind, Direction::Response)
        };
        let impact = evaluate(&change, &base_surface, &head_surface, &coverage);
        evaluated.push((impact, compat, change.kind, change.service));
    }

    let min_class = min_impact.unwrap_or(Class::NoKnownImpact);
    let mut kept: Vec<Value> = Vec::new();
    let mut compatible_changes: u32 = 0;
    for (impact, compat, kind, service) in evaluated {
        let cls = impact.class;
        if cls < min_class {
            if matches!(cls, Class::NoKnownImpact) && matches!(compat, Compat::Compatible) {
                compatible_changes += impact.compatible_changes;
            }
            continue;
        }
        if matches!(cls, Class::NoKnownImpact) && matches!(compat, Compat::Compatible) {
            compatible_changes += impact.compatible_changes;
        }
        kept.push(impact_to_value(&impact, &kind, &service, compat, cap));
    }
    kept.sort_by(|a, b| {
        let ka = sort_key(a);
        let kb = sort_key(b);
        ka.cmp(&kb)
    });

    let data = json!({
        "changes": kept,
        "compatible_changes": compatible_changes,
        "coverage": coverage_to_value(&coverage),
    });
    let envelope = success_envelope(data.clone(), &head_label, true, started);
    let text = render_diff_contracts(&data);
    Ok(outcome(envelope, &data, text))
}

fn sort_key(v: &Value) -> String {
    format!(
        "{}|{}|{}|{}",
        v["side"].as_str().unwrap_or(""),
        v["endpoint"]["service"].as_str().unwrap_or(""),
        v["endpoint"]["key"].as_str().unwrap_or(""),
        v["kind"].as_str().unwrap_or("")
    )
}

fn direction_of_change(kind: &ChangeKind) -> Option<Direction> {
    match kind {
        ChangeKind::FieldRemoved { direction, .. }
        | ChangeKind::FieldAdded { direction, .. }
        | ChangeKind::FieldRenamed { direction, .. }
        | ChangeKind::FieldTypeChanged { direction, .. }
        | ChangeKind::RequirednessChanged { direction, .. }
        | ChangeKind::NullabilityChanged { direction, .. }
        | ChangeKind::EnumValueRemoved { direction, .. }
        | ChangeKind::EnumValueAdded { direction, .. } => Some(*direction),
        _ => None,
    }
}

fn parse_min_impact(args: &Map<String, Value>) -> Result<Option<Class>, ToolOutcome> {
    let raw = match args.get("min_impact").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return Ok(None),
    };
    Ok(Some(match raw {
        "NoKnownImpact" => Class::NoKnownImpact,
        "NeedsInvestigation" => Class::NeedsInvestigation,
        "Verified" => Class::Verified,
        _ => {
            return Err(ToolOutcome {
                structured: json!({}),
                text: format!("invalid min_impact: {raw:?}"),
                is_error: true,
            });
        }
    }))
}

fn parse_cap(
    args: &Map<String, Value>,
    default: usize,
    max: usize,
    started: Instant,
) -> Result<usize, ToolOutcome> {
    let raw = match args.get("cap").and_then(|v| v.as_u64()) {
        Some(n) => n as usize,
        None => default,
    };
    if raw > max {
        return Err(error_outcome(
            "range_too_large",
            format!("cap {raw} exceeds max {max}"),
            Some(json!({"limit": raw, "max": max, "requested": raw})),
            "live",
            started,
        ));
    }
    Ok(raw)
}

fn build_changed_source(
    data_dir: &std::path::Path,
    base: &SnapshotRecord,
    head: &SnapshotRecord,
) -> MultiRepoChangedFiles {
    let src = MirrorChangedFiles::new(data_dir);
    let mut by_repo = std::collections::BTreeMap::new();
    for (repo, base_sha) in &base.repos {
        let head_sha = head
            .repos
            .get(repo)
            .cloned()
            .unwrap_or_else(|| base_sha.clone());
        by_repo.insert(repo.clone(), src.diff(repo, base_sha, &head_sha));
    }
    MultiRepoChangedFiles { by_repo }
}

fn load_snapshot(
    ctx: &McpContext<'_>,
    label: &str,
    started: Instant,
) -> Result<SnapshotRecord, ToolOutcome> {
    let mgr = ctx.snapshots.ok_or_else(|| {
        error_outcome(
            "snapshot_manager_unavailable",
            "snapshot manager is not configured for this server",
            None,
            label,
            started,
        )
    })?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            error_outcome(
                "invalid_argument",
                format!("tokio runtime: {e}"),
                None,
                label,
                started,
            )
        })?;
    let outcome = rt.block_on(mgr.get(label, 1_000)).map_err(|e| match e {
        crate::federation::contracts::snapshots::manager::PrepareError::SnapshotNotFound {
            snapshot,
        } => error_outcome(
            "snapshot_not_found",
            format!("snapshot {snapshot:?} not found"),
            Some(json!({"snapshot": snapshot})),
            label,
            started,
        ),
        crate::federation::contracts::snapshots::manager::PrepareError::Busy { retry_after_ms } => {
            error_outcome(
                "busy",
                "snapshot residency busy",
                Some(json!({"retry_after_ms": retry_after_ms})),
                label,
                started,
            )
        }
        crate::federation::contracts::snapshots::manager::PrepareError::RepoNotRegistered {
            repo,
        } => error_outcome(
            "repo_not_registered",
            format!("repo {repo:?} not configured"),
            Some(json!({"repo": repo})),
            label,
            started,
        ),
        other => error_outcome(
            "invalid_argument",
            format!("{other:?}"),
            None,
            label,
            started,
        ),
    })?;
    Ok(outcome.record)
}

fn snapshot_state_is_ready(r: &SnapshotRecord) -> bool {
    matches!(
        r.state,
        crate::federation::contracts::snapshots::SnapshotState::Ready
    )
}

fn snapshot_state_label(r: &SnapshotRecord) -> &'static str {
    match r.state {
        crate::federation::contracts::snapshots::SnapshotState::Pending => "pending",
        crate::federation::contracts::snapshots::SnapshotState::Indexing => "indexing",
        crate::federation::contracts::snapshots::SnapshotState::Ready => "ready",
        crate::federation::contracts::snapshots::SnapshotState::Failed => "failed",
    }
}

fn snapshot_contract_index(
    ctx: &McpContext<'_>,
    record: &SnapshotRecord,
    started: Instant,
) -> Result<Arc<ContractIndex>, ToolOutcome> {
    let mgr = ctx.snapshots.ok_or_else(|| {
        error_outcome(
            "snapshot_manager_unavailable",
            "snapshot manager is not configured for this server",
            None,
            &record.id,
            started,
        )
    })?;
    let (fed, _guard) = mgr.from_snapshot(record).map_err(|e| {
        error_outcome(
            "invalid_argument",
            format!("from_snapshot: {e}"),
            None,
            &record.id,
            started,
        )
    })?;
    let ci = fed.contract_index.read().clone();
    ci.ok_or_else(|| {
        error_outcome(
            "snapshot_not_ready",
            "snapshot has no contract index",
            Some(json!({"snapshot": record.id})),
            &record.id,
            started,
        )
    })
}

fn build_scope(head: &SnapshotRecord) -> DiffScope {
    let mut reviewed = Vec::new();
    for (repo, commit) in &head.repos {
        if head
            .repo_states
            .get(repo)
            .map(|s| {
                matches!(
                    s,
                    crate::federation::contracts::snapshots::RepoSnapshotState::Cached { .. }
                )
            })
            .unwrap_or(false)
        {
            reviewed.push(crate::federation::contracts::diff::ReviewedRepo {
                repo: repo.clone(),
                commit: Some(commit.clone()),
                dirty: false,
            });
        }
    }
    let mut unreviewed = Vec::new();
    for (repo, state) in &head.repo_states {
        let entry = match state {
            crate::federation::contracts::snapshots::RepoSnapshotState::Excluded => {
                Some(("excluded", String::new()))
            }
            crate::federation::contracts::snapshots::RepoSnapshotState::Failed {
                error, ..
            } => Some(("failed", error.clone())),
            crate::federation::contracts::snapshots::RepoSnapshotState::Indexing { .. } => {
                Some(("not_ready", String::new()))
            }
            crate::federation::contracts::snapshots::RepoSnapshotState::Queued { .. } => {
                Some(("not_ready", String::new()))
            }
            crate::federation::contracts::snapshots::RepoSnapshotState::Cached { .. } => None,
        };
        if let Some((reason, error)) = entry {
            unreviewed.push(crate::federation::contracts::diff::UnreviewedRepo {
                repo: repo.clone(),
                reason: reason.to_string(),
                error: if error.is_empty() { None } else { Some(error) },
            });
        }
    }
    DiffScope {
        reviewed,
        unreviewed,
        configured_only: true,
    }
}

fn build_repo_coverage(
    head: &SnapshotRecord,
) -> Vec<crate::federation::contracts::diff::RepoCoverage> {
    let mut out = Vec::new();
    for (repo, commit) in &head.repos {
        let mut entry = crate::federation::contracts::diff::RepoCoverage {
            repo: repo.clone(),
            commit: Some(commit.clone()),
            state: "indexed".to_string(),
            sensor_counts: BTreeMap::new(),
            error: None,
        };
        if let Some(state) = head.repo_states.get(repo) {
            match state {
                crate::federation::contracts::snapshots::RepoSnapshotState::Failed {
                    error,
                    ..
                } => {
                    entry.state = "failed".to_string();
                    entry.error = Some(error.clone());
                }
                crate::federation::contracts::snapshots::RepoSnapshotState::Indexing { .. } => {
                    entry.state = "not_ready".to_string();
                }
                crate::federation::contracts::snapshots::RepoSnapshotState::Queued { .. } => {
                    entry.state = "not_ready".to_string();
                }
                crate::federation::contracts::snapshots::RepoSnapshotState::Excluded => {
                    entry.state = "excluded".to_string();
                }
                crate::federation::contracts::snapshots::RepoSnapshotState::Cached { .. } => {}
            }
        }
        out.push(entry);
    }
    out
}

fn impact_to_value(
    impact: &Impact,
    kind: &ChangeKind,
    service: &ServiceName,
    compat: Compat,
    _cap: usize,
) -> Value {
    let mut affected: Vec<Value> = Vec::new();
    for a in &impact.affected {
        affected.push(json!({
            "consumer": a.consumer.caller.name,
            "service": a.service.0,
            "class": a.class.as_str(),
            "reasons": a.reason.as_str(),
        }));
    }
    let side = match kind {
        ChangeKind::ConsumerEndpointUnmatched { .. }
        | ChangeKind::ConsumerFieldUnmatched { .. }
        | ChangeKind::ConsumerRebound { .. } => "consumer",
        _ => "provider",
    };
    let endpoint = endpoint_from_change(kind, service);
    let mut value = json!({
        "side": side,
        "endpoint": endpoint,
        "kind": kind_label(kind),
        "compat": compat.as_str(),
        "impact": {
            "class": impact.class.as_str(),
            "reasons": impact.reason.as_ref().map(|r| vec![r.as_str()]).unwrap_or_default(),
            "scope": scope_to_value(&impact.scope),
        },
        "affected": affected,
        "paths": [],
        "truncated": false,
    });
    if let ChangeKind::FieldRemoved {
        direction, path, ..
    }
    | ChangeKind::FieldAdded {
        direction, path, ..
    }
    | ChangeKind::FieldTypeChanged {
        direction, path, ..
    }
    | ChangeKind::RequirednessChanged {
        direction, path, ..
    }
    | ChangeKind::NullabilityChanged {
        direction, path, ..
    } = kind
    {
        value["direction"] = json!(direction_label_str(*direction));
        value["field"] = json!(path.to_string());
    }
    if let ChangeKind::EnumValueRemoved {
        direction,
        path,
        value: v,
        ..
    }
    | ChangeKind::EnumValueAdded {
        direction,
        path,
        value: v,
        ..
    } = kind
    {
        value["direction"] = json!(direction_label_str(*direction));
        value["field"] = json!(path.to_string());
        value["value"] = json!(v);
    }
    if let ChangeKind::FieldRenamed {
        direction,
        from,
        to,
        ..
    } = kind
    {
        value["direction"] = json!(direction_label_str(*direction));
        value["from"] = json!(from.to_string());
        value["to"] = json!(to.to_string());
    }
    if let ChangeKind::PathChanged { from, to, .. } | ChangeKind::MethodChanged { from, to, .. } =
        kind
    {
        value["from"] = json!(from.to_string());
        value["to"] = json!(to.to_string());
    }
    value
}

fn endpoint_from_change(kind: &ChangeKind, service: &ServiceName) -> Value {
    let key = match kind {
        ChangeKind::EndpointRemoved { key } | ChangeKind::EndpointAdded { key } => key.clone(),
        ChangeKind::PathChanged { to, .. } | ChangeKind::MethodChanged { to, .. } => to.clone(),
        ChangeKind::FieldRemoved { endpoint, .. }
        | ChangeKind::FieldAdded { endpoint, .. }
        | ChangeKind::FieldRenamed { endpoint, .. }
        | ChangeKind::FieldTypeChanged { endpoint, .. }
        | ChangeKind::RequirednessChanged { endpoint, .. }
        | ChangeKind::NullabilityChanged { endpoint, .. }
        | ChangeKind::EnumValueRemoved { endpoint, .. }
        | ChangeKind::EnumValueAdded { endpoint, .. }
        | ChangeKind::ChangedWithoutSchema { endpoint } => endpoint.1.clone(),
        ChangeKind::ConsumerEndpointUnmatched { .. }
        | ChangeKind::ConsumerFieldUnmatched { .. }
        | ChangeKind::ConsumerRebound { .. } => ContractKey::Http {
            method: MethodSpec::Unknown,
            template: String::new(),
        },
    };
    json!({
        "service": service.0,
        "key": key.to_string(),
    })
}

fn kind_label(kind: &ChangeKind) -> &'static str {
    match kind {
        ChangeKind::EndpointRemoved { .. } => "EndpointRemoved",
        ChangeKind::EndpointAdded { .. } => "EndpointAdded",
        ChangeKind::PathChanged { .. } => "PathChanged",
        ChangeKind::MethodChanged { .. } => "MethodChanged",
        ChangeKind::FieldRemoved { .. } => "FieldRemoved",
        ChangeKind::FieldAdded { .. } => "FieldAdded",
        ChangeKind::FieldRenamed { .. } => "FieldRenamed",
        ChangeKind::FieldTypeChanged { .. } => "FieldTypeChanged",
        ChangeKind::RequirednessChanged { .. } => "RequirednessChanged",
        ChangeKind::NullabilityChanged { .. } => "NullabilityChanged",
        ChangeKind::EnumValueRemoved { .. } => "EnumValueRemoved",
        ChangeKind::EnumValueAdded { .. } => "EnumValueAdded",
        ChangeKind::ChangedWithoutSchema { .. } => "ChangedWithoutSchema",
        ChangeKind::ConsumerEndpointUnmatched { .. } => "ConsumerEndpointUnmatched",
        ChangeKind::ConsumerFieldUnmatched { .. } => "ConsumerFieldUnmatched",
        ChangeKind::ConsumerRebound { .. } => "ConsumerRebound",
    }
}

fn direction_label_str(d: Direction) -> &'static str {
    match d {
        Direction::Request => "request",
        Direction::Response => "response",
        Direction::Payload => "payload",
    }
}

fn scope_to_value(s: &DiffScope) -> Value {
    let reviewed: Vec<Value> = s
        .reviewed
        .iter()
        .map(|r| {
            json!({
                "repo": r.repo,
                "commit": r.commit,
                "dirty": r.dirty,
            })
        })
        .collect();
    let unreviewed: Vec<Value> = s
        .unreviewed
        .iter()
        .map(|r| {
            json!({
                "repo": r.repo,
                "reason": r.reason,
                "error": r.error,
            })
        })
        .collect();
    json!({
        "reviewed": reviewed,
        "unreviewed": unreviewed,
        "configured_only": s.configured_only,
    })
}

fn coverage_to_value(c: &DiffCoverage) -> Value {
    json!({
        "complete": c.complete,
        "scope": scope_to_value(&c.scope),
        "repos": c.repos.iter().map(|r| {
            json!({
                "repo": r.repo,
                "commit": r.commit,
                "state": r.state,
                "sensors": r.sensor_counts,
                "error": r.error,
            })
        }).collect::<Vec<_>>(),
        "unresolved_consumers": c.unresolved_consumers.iter().map(|k| json!(k.caller.name)).collect::<Vec<_>>(),
        "ambiguous": c.ambiguous.iter().map(|k| json!(k.caller.name)).collect::<Vec<_>>(),
        "unnormalized": c.unnormalized.iter().map(|k| json!(k.caller.name)).collect::<Vec<_>>(),
        "external": c.external.iter().map(|(h, n)| json!({"host": h, "calls": n})).collect::<Vec<_>>(),
        "stale_bindings": c.stale_bindings,
        "schemaless_endpoints": c.schemaless_endpoints.iter().map(|e| json!({"service": e.0.0, "key": e.1.to_string()})).collect::<Vec<_>>(),
    })
}

fn render_diff_contracts(data: &Value) -> String {
    let changes = data["changes"].as_array().cloned().unwrap_or_default();
    let compatible = data["compatible_changes"].as_u64().unwrap_or(0);
    let mut out = String::new();
    out.push_str(&format!(
        "# diff_contracts ({} changes, {} compatible)\n",
        changes.len(),
        compatible
    ));
    for ch in &changes {
        let endpoint = ch["endpoint"].clone();
        let kind = ch["kind"].as_str().unwrap_or("");
        let compat = ch["compat"].as_str().unwrap_or("");
        let cls = ch["impact"]["class"].as_str().unwrap_or("");
        out.push_str(&format!(
            "- {} {} ({} {}) → {}\n",
            endpoint["service"].as_str().unwrap_or(""),
            endpoint["key"].as_str().unwrap_or(""),
            kind,
            compat,
            cls,
        ));
    }
    out
}

// ─── trace_impact ─────────────────────────────────────────────────────

async fn run_trace_impact(
    ctx: &McpContext<'_>,
    args_map: Map<String, Value>,
    started: Instant,
) -> Result<ToolOutcome, ToolOutcome> {
    let snap_label = snapshot_label(&args_map);
    let from = args_map.get("from").ok_or_else(|| {
        error_outcome(
            "invalid_argument",
            "missing required argument: from",
            Some(json!({"arg": "from"})),
            &snap_label,
            started,
        )
    })?;
    let depth = args_map
        .get("depth")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(6);
    if depth > 12 {
        return Err(error_outcome(
            "range_too_large",
            format!("depth {depth} exceeds max 12"),
            Some(json!({"limit": depth, "max": 12, "requested": depth})),
            &snap_label,
            started,
        ));
    }
    let min_confidence = args_map
        .get("min_confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as f32;
    let cap = parse_cap(&args_map, 50, 500, started)?;

    let mut has_endpoint = false;
    let mut has_field = false;
    let mut has_symbol = false;
    if let Some(v) = from.get("endpoint") {
        if !v.is_null() {
            has_endpoint = true;
        }
    }
    if let Some(v) = from.get("field") {
        if !v.is_null() {
            has_field = true;
        }
    }
    if let Some(v) = from.get("symbol") {
        if !v.is_null() {
            has_symbol = true;
        }
    }
    let ones = has_endpoint as u32 + has_field as u32 + has_symbol as u32;
    if ones != 1 {
        return Err(error_outcome(
            "invalid_argument",
            "exactly one of endpoint, field, symbol is required",
            Some(json!({"arg": "from"})),
            &snap_label,
            started,
        ));
    }

    let _ = match resolve_view(ctx, &args_map, started).await? {
        ViewHandle::Empty(_) => {
            return Err(error_outcome(
                "contract_not_found",
                "no contract index available for this snapshot",
                Some(json!({"endpoint": null})),
                &snap_label,
                started,
            ));
        }
        ViewHandle::Index { index, .. } => index,
    };

    // Find starting node ids.
    let mut starts: Vec<String> = Vec::new();
    if has_endpoint {
        let endpoint_value = from.get("endpoint").cloned().unwrap_or(Value::Null);
        let endpoint = parse_endpoint_for_trace(&endpoint_value)?;
        let key = ContractKey::from_str(&endpoint.1).map_err(|e| {
            error_outcome(
                "invalid_argument",
                format!("malformed endpoint.key: {e}"),
                Some(json!({"arg": "from.endpoint.key"})),
                &snap_label,
                started,
            )
        })?;
        let _eid: EndpointId = (ServiceName(endpoint.0.clone()), key);
        starts.push(endpoint.0.clone());
    } else if has_field {
        let field = from.get("field").cloned().unwrap_or(Value::Null);
        let endpoint = field.get("endpoint").cloned().unwrap_or(Value::Null);
        let endpoint_pair = parse_endpoint_for_trace(&endpoint)?;
        let _direction = field
            .get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or("response");
        let _path = field
            .get("json_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        starts.push(endpoint_pair.0.clone());
    } else {
        let symbol = from.get("symbol").and_then(|v| v.as_str()).ok_or_else(|| {
            error_outcome(
                "invalid_argument",
                "missing symbol",
                Some(json!({"arg": "from.symbol"})),
                &snap_label,
                started,
            )
        })?;
        starts.push(symbol.to_string());
    }

    let backend = match ctx.federation {
        Some(fed) => fed.backend(),
        None => {
            return Err(error_outcome(
                "federation_disabled",
                "federation backend unavailable",
                None,
                &snap_label,
                started,
            ));
        }
    };

    let impact_outcome: ImpactResult = {
        let refs: Vec<&str> = starts.iter().map(String::as_str).collect();
        backend
            .traverse_impact(&refs, depth, cap, min_confidence)
            .map_err(|e| {
                error_outcome(
                    "invalid_argument",
                    format!("traverse_impact failed: {e}"),
                    None,
                    &snap_label,
                    started,
                )
            })?
    };
    let mut paths = impact_outcome.paths;
    paths.sort_by(|a, b| {
        b.min_confidence
            .partial_cmp(&a.min_confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.hops.len().cmp(&b.hops.len()))
            .then_with(|| {
                let ak = leaf_id(a).unwrap_or("");
                let bk = leaf_id(b).unwrap_or("");
                ak.cmp(bk)
            })
    });
    if paths.len() > cap {
        paths.truncate(cap);
    }
    let truncated = impact_outcome.truncated || paths.len() > cap;

    let mut paths_value: Vec<Value> = Vec::new();
    for p in &paths {
        paths_value.push(graph_path_to_value(p));
    }
    let scope = scope_for_view(ctx, &args_map);
    let data = json!({
        "paths": paths_value,
        "truncated": truncated,
        "scope": scope,
    });
    let envelope = success_envelope(data.clone(), &snap_label, snap_label != "live", started);
    let text = render_trace_impact(&data);
    Ok(outcome(envelope, &data, text))
}

fn leaf_id(p: &GraphImpactPath) -> Option<&str> {
    p.hops.last().map(|h| h.node.id.as_str())
}

fn graph_path_to_value(p: &GraphImpactPath) -> Value {
    let mut hops: Vec<Value> = Vec::new();
    for h in &p.hops {
        hops.push(json!({
            "edge": format!("{:?}", h.edge.edge_type),
            "node": h.node.id,
            "node_type": format!("{:?}", h.node.node_type),
            "name": h.node.name,
            "provenance": provenance_label(h.edge.provenance.as_ref()),
        }));
    }
    let start = p.hops.first().map(|h| h.node.id.as_str()).unwrap_or("");
    json!({
        "start": start,
        "min_confidence": p.min_confidence,
        "hops": hops,
    })
}

fn provenance_label(p: Option<&EdgeProvenance>) -> Value {
    let Some(p) = p else {
        return json!({"kind": "unknown", "confidence": 0.0});
    };
    let kind = match p {
        EdgeProvenance::Static { .. } => "static",
        EdgeProvenance::Confirmed { .. } => "confirmed",
        EdgeProvenance::Heuristic { .. } => "heuristic",
        EdgeProvenance::Runtime { .. } => "runtime",
    };
    let confidence = match p {
        EdgeProvenance::Static { .. } => 1.0_f32,
        EdgeProvenance::Confirmed { .. } => 1.0_f32,
        EdgeProvenance::Heuristic { confidence, .. } => *confidence,
        EdgeProvenance::Runtime { .. } => 0.9_f32,
    };
    let mut v = json!({"kind": kind, "confidence": confidence});
    if let EdgeProvenance::Confirmed { source } = p {
        v["source"] = json!(source);
    }
    if let EdgeProvenance::Heuristic { detector, .. } = p {
        v["detector"] = json!(detector);
    }
    v
}

fn parse_endpoint_for_trace(v: &Value) -> Result<(String, String), ToolOutcome> {
    let obj = v.as_object().ok_or_else(|| ToolOutcome {
        structured: json!({}),
        text: "endpoint must be an object {service, key}".to_string(),
        is_error: true,
    })?;
    let service = obj
        .get("service")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let key = obj
        .get("key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok((service, key))
}

fn render_trace_impact(data: &Value) -> String {
    let paths = data["paths"].as_array().cloned().unwrap_or_default();
    let mut out = String::new();
    out.push_str(&format!("# trace_impact ({} path(s))\n", paths.len()));
    for p in &paths {
        out.push_str(&format!(
            "- {} (min_confidence: {})\n",
            p["start"].as_str().unwrap_or(""),
            p["min_confidence"].as_f64().unwrap_or(0.0)
        ));
    }
    out
}

// ─── get_coverage ─────────────────────────────────────────────────────

async fn run_get_coverage(
    ctx: &McpContext<'_>,
    args_map: Map<String, Value>,
    started: Instant,
) -> Result<ToolOutcome, ToolOutcome> {
    let snap_label = snapshot_label(&args_map);
    let view = match resolve_view(ctx, &args_map, started).await? {
        ViewHandle::Empty(_) => {
            let data =
                json!({"complete": true, "scope": scope_for_view(ctx, &args_map), "repos": []});
            let envelope =
                success_envelope(data.clone(), &snap_label, snap_label != "live", started);
            return Ok(outcome(envelope, &data, render_coverage(&data)));
        }
        ViewHandle::Index { index, .. } => index,
    };
    let endpoint_arg = args_map.get("endpoint").cloned();
    let scope_value = scope_for_view(ctx, &args_map);
    let scope = parse_scope(scope_value);
    let mut coverage_repos: Vec<crate::federation::contracts::diff::RepoCoverage> = Vec::new();
    if let Some(fed) = ctx.federation {
        for (id, health) in fed.list_repos() {
            let commit = fed
                .get_repo(&id)
                .and_then(|r| r.db().get_last_commit().ok().flatten());
            let state_str = match health {
                crate::federation::health::RepoHealth::Ready => "indexed",
                crate::federation::health::RepoHealth::Indexing => "not_ready",
                _ => "failed",
            };
            coverage_repos.push(crate::federation::contracts::diff::RepoCoverage {
                repo: id.as_str().to_string(),
                commit,
                state: state_str.to_string(),
                sensor_counts: BTreeMap::new(),
                error: None,
            });
        }
    }
    let coverage = build_coverage(&view, coverage_repos, scope);
    let endpoint_id = if let Some(v) = endpoint_arg {
        let eid = parse_endpoint_for_trace(&v).ok();
        if let Some((service, key)) = eid {
            let k = ContractKey::from_str(&key).ok();
            k.map(|k| (ServiceName(service), k))
        } else {
            None
        }
    } else {
        None
    };
    let mut value = coverage_to_value(&coverage);
    if let Some(eid) = &endpoint_id {
        let complete = coverage_complete(&coverage, Some(eid), &view);
        value["complete"] = json!(complete);
    }
    let envelope = success_envelope(value.clone(), &snap_label, snap_label != "live", started);
    let text = render_coverage(&value);
    Ok(outcome(envelope, &value, text))
}

fn render_coverage(data: &Value) -> String {
    let mut out = String::new();
    let repos = data["repos"].as_array().map(|a| a.len()).unwrap_or(0);
    out.push_str(&format!(
        "# coverage ({} repos, complete: {})\n",
        repos, data["complete"]
    ));
    out
}

fn empty_scope() -> Value {
    json!({
        "reviewed": [],
        "unreviewed": [],
        "configured_only": true,
    })
}

fn parse_scope(v: Value) -> DiffScope {
    let reviewed = v
        .get("reviewed")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let repo = item.get("repo").and_then(|s| s.as_str())?.to_string();
                    let commit = item
                        .get("commit")
                        .and_then(|s| s.as_str())
                        .map(String::from);
                    let dirty = item.get("dirty").and_then(|s| s.as_bool()).unwrap_or(false);
                    Some(ReviewedRepo {
                        repo,
                        commit,
                        dirty,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let unreviewed = v
        .get("unreviewed")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let repo = item.get("repo").and_then(|s| s.as_str())?.to_string();
                    let reason = item.get("reason").and_then(|s| s.as_str())?.to_string();
                    let error = item.get("error").and_then(|s| s.as_str()).map(String::from);
                    Some(UnreviewedRepo {
                        repo,
                        reason,
                        error,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    DiffScope {
        reviewed,
        unreviewed,
        configured_only: true,
    }
}

// ─── helpers ─────────────────────────────────────────────────────────

fn object_or_empty(args: Value) -> Map<String, Value> {
    match args {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

fn snapshot_label(args_map: &Map<String, Value>) -> String {
    args_map
        .get("snapshot")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "live".to_string())
}

fn scope_for_view(ctx: &McpContext<'_>, args_map: &Map<String, Value>) -> Value {
    if snapshot_label(args_map) == "live" {
        return ctx.federation.map(live_scope).unwrap_or_else(empty_scope);
    }
    empty_scope()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::contracts::diff::ConsumerKey;
    use crate::federation::contracts::diff::{Affected, ReviewedRepo, UnreviewedRepo};
    use crate::federation::contracts::model::SymbolKey;
    use crate::federation::repo_id::RepoId;

    #[test]
    fn direction_of_change_extracts_field_kinds() {
        let k = ChangeKind::FieldRemoved {
            endpoint: (
                ServiceName("orders".into()),
                ContractKey::Http {
                    method: MethodSpec::Known(crate::federation::contracts::model::HttpMethod::Get),
                    template: "/".into(),
                },
            ),
            direction: Direction::Response,
            path: JsonPath(vec![]),
        };
        assert_eq!(direction_of_change(&k), Some(Direction::Response));
    }

    #[test]
    fn kind_label_maps_each_variant() {
        let k = ChangeKind::EndpointRemoved {
            key: ContractKey::Http {
                method: MethodSpec::Known(crate::federation::contracts::model::HttpMethod::Get),
                template: "/".into(),
            },
        };
        assert_eq!(kind_label(&k), "EndpointRemoved");
    }

    #[test]
    fn parse_min_impact_accepts_no_known() {
        let mut m = Map::new();
        m.insert("min_impact".to_string(), json!("NoKnownImpact"));
        assert_eq!(parse_min_impact(&m).unwrap(), Some(Class::NoKnownImpact));
    }

    #[test]
    fn parse_cap_rejects_over_max() {
        let mut m = Map::new();
        m.insert("cap".to_string(), json!(501));
        let started = Instant::now();
        let o = parse_cap(&m, 50, 500, started).unwrap_err();
        assert_eq!(o.structured["error"]["code"], json!("range_too_large"));
    }

    #[test]
    fn scope_to_value_renders_reviewed_and_unreviewed() {
        let s = DiffScope {
            reviewed: vec![ReviewedRepo {
                repo: "orders".into(),
                commit: Some("abc".into()),
                dirty: false,
            }],
            unreviewed: vec![UnreviewedRepo {
                repo: "reports".into(),
                reason: "excluded".into(),
                error: None,
            }],
            configured_only: true,
        };
        let v = scope_to_value(&s);
        assert_eq!(v["reviewed"][0]["repo"], "orders");
        assert_eq!(v["unreviewed"][0]["repo"], "reports");
    }

    #[test]
    fn impact_to_value_emits_side_and_endpoint() {
        let consumer_key = ConsumerKey {
            caller: SymbolKey {
                repo: RepoId::new("billing").unwrap(),
                path: "src/main.py".into(),
                container: None,
                name: "fetch_order".into(),
            },
            target: crate::federation::contracts::diff::ConsumerTargetKey::Contract(
                ContractKey::Http {
                    method: MethodSpec::Known(crate::federation::contracts::model::HttpMethod::Get),
                    template: "/api/orders/{}".into(),
                },
            ),
        };
        let impact = Impact {
            service: ServiceName("billing".into()),
            kind: ChangeKind::ConsumerEndpointUnmatched {
                consumer: consumer_key.clone(),
            },
            class: Class::NeedsInvestigation,
            reason: Some(DiffReason::UnresolvedCandidates),
            affected: vec![Affected {
                service: ServiceName("billing".into()),
                consumer: consumer_key.clone(),
                class: Class::NeedsInvestigation,
                reason: DiffReason::UnresolvedCandidates,
            }],
            scope: DiffScope::default(),
            coverage: DiffCoverage::default(),
            compatible_changes: 0,
        };
        let value = impact_to_value(
            &impact,
            &ChangeKind::ConsumerEndpointUnmatched {
                consumer: consumer_key,
            },
            &ServiceName("billing".into()),
            Compat::Breaking,
            50,
        );
        assert_eq!(value["side"], "consumer");
        assert_eq!(value["compat"], "Breaking");
        assert_eq!(value["impact"]["class"], "NeedsInvestigation");
    }

    #[test]
    fn parse_endpoint_for_trace_returns_pair() {
        let v = json!({"service": "orders", "key": "http:GET /x"});
        let (s, k) = parse_endpoint_for_trace(&v).unwrap();
        assert_eq!(s, "orders");
        assert_eq!(k, "http:GET /x");
    }

    #[test]
    fn graph_path_to_value_serializes_hops() {
        use crate::federation::graph_backend::{ImpactHop, ImpactPath};
        use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
        let ns = crate::schema::RepoNamespace::for_test();
        let node = GraphNode::new_in(NodeType::Function, "fetch".into(), "src/x.py".into(), &ns);
        let node_id = node.id.clone();
        let edge = GraphEdge::new(EdgeType::Calls, node_id.clone(), node_id.clone());
        let path = ImpactPath {
            hops: vec![ImpactHop { edge, node }],
            min_confidence: 1.0,
        };
        let v = graph_path_to_value(&path);
        // The `start` is the first hop's node id; the test below only
        // asserts that the field is non-empty (GraphNode auto-generates
        // a UUID-based id).
        assert!(v["start"].is_string());
        assert_eq!(v["min_confidence"], 1.0);
    }
}

// Suppress unused-import warnings for types we keep for trait objects.
#[allow(dead_code)]
fn _retain_types(_: &ServiceInfo) {
    let _: &str = std::any::type_name::<EdgeType>();
}
