//! Analysis tools: `diff_contracts`, `trace_impact`, `get_coverage`
//! (`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §10.1, §12, §13).
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

use super::envelope::{
    check_api_version, error_outcome, outcome, success_envelope, success_envelope_with_view,
};
use super::view::resolve_view;
use super::{ContractToolEntry, ContractToolFuture, ToolOutcome};
use crate::federation::contracts::changed_files::{MirrorChangedFiles, MultiRepoChangedFiles};
use crate::federation::contracts::diff::{
    build_coverage, classify, coverage_complete, diff_consumers, diff_contracts, evaluate,
    ChangeKind, Class, Compat, ContractSurface, Coverage as DiffCoverage, Impact, ReviewedRepo,
    Scope as DiffScope, UnreviewedRepo,
};
use crate::federation::contracts::index::{ContractIndex, EndpointId};
use crate::federation::contracts::model::ServiceName;
use crate::federation::contracts::model::{ContractKey, Direction, JsonPath, MethodSpec};
use crate::federation::contracts::snapshots::record::SnapshotRecord;
use crate::federation::graph_backend::{GraphBackend, ImpactPath as GraphImpactPath, ImpactResult};
use crate::federation::repo_id::GlobalId;
use crate::schema::EdgeProvenance;
use crate::server::mcp::handler::McpContext;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
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
    // §12: `repo` and `service` narrow the reported changes; both
    // apply before `min_impact` and before `compatible_changes`
    // counting (a filtered-out change is not counted either).
    let service_filter = args_map
        .get("service")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let repo_filter = args_map
        .get("repo")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let base_record = load_snapshot(ctx, &base_label, started).await?;
    let head_record = load_snapshot(ctx, &head_label, started).await?;
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
    let (base_index, base_fed, _base_hold) =
        snapshot_contract_index(ctx, &base_record, &args_map, started)?;
    let (head_index, head_fed, _head_hold) =
        snapshot_contract_index(ctx, &head_record, &args_map, started)?;
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
    let (coverage_repos, repo_coverages) =
        build_repo_coverage_with_ledgers(&data_dir, &head_record);
    let mut coverage = build_coverage(&head_index, coverage_repos, scope.clone());
    coverage.repo_coverages = repo_coverages;

    for repo in changed_source.unavailable_repos() {
        if !coverage.scope.unreviewed.iter().any(|u| u.repo == repo) {
            coverage
                .scope
                .unreviewed
                .push(crate::federation::contracts::diff::UnreviewedRepo {
                    repo: repo.clone(),
                    reason: "diff_unavailable".to_string(),
                    error: Some("git mirror missing or diff failed".to_string()),
                });
        }
        if let Some(cover) = coverage.repo_coverages.get_mut(&repo) {
            cover.error = Some("git diff unavailable".to_string());
        }
    }

    coverage.complete = coverage_complete(&coverage, None, &head_index);

    // §15.2 scenario 12: a rename whose type changed splits into
    // `FieldRemoved` + `FieldAdded` (the wire kinds §12 lists), but
    // the pair still *is* a rename for §9.4/§9.5 — both halves
    // classify as `BreakingIfRead` and evaluate with the rename's
    // "consumer reads the old name" rule. When exactly one field
    // was removed and one added under the same endpoint +
    // direction, the `FieldAdded` half is evaluated as the
    // `FieldRenamed` it belongs to while the wire keeps its
    // `FieldAdded` kind.
    let mut removed_by_ed: BTreeMap<(EndpointId, Direction), Vec<JsonPath>> = BTreeMap::new();
    let mut added_by_ed: BTreeMap<(EndpointId, Direction), Vec<JsonPath>> = BTreeMap::new();
    for change in &all_changes {
        match &change.kind {
            ChangeKind::FieldRemoved {
                endpoint,
                direction,
                path,
                ..
            } => {
                removed_by_ed
                    .entry((endpoint.clone(), *direction))
                    .or_default()
                    .push(path.clone());
            }
            ChangeKind::FieldAdded {
                endpoint,
                direction,
                path,
                ..
            } => {
                added_by_ed
                    .entry((endpoint.clone(), *direction))
                    .or_default()
                    .push(path.clone());
            }
            _ => {}
        }
    }
    let mut rename_splits: BTreeMap<(EndpointId, Direction), (JsonPath, JsonPath)> =
        BTreeMap::new();
    for (ed, removes) in &removed_by_ed {
        if let Some(adds) = added_by_ed.get(ed) {
            if removes.len() == 1 && adds.len() == 1 {
                rename_splits.insert(ed.clone(), (removes[0].clone(), adds[0].clone()));
            }
        }
    }

    let mut evaluated: Vec<(Impact, Compat, ChangeKind, ServiceName)> = Vec::new();
    for change in all_changes {
        let mut eval_kind = change.kind.clone();
        if let ChangeKind::FieldAdded {
            endpoint,
            direction,
            path,
            required,
            ..
        } = &change.kind
        {
            if let Some((from, to)) = rename_splits.get(&(endpoint.clone(), *direction)) {
                if to == path {
                    eval_kind = ChangeKind::FieldRenamed {
                        endpoint: endpoint.clone(),
                        direction: *direction,
                        from: from.clone(),
                        to: path.clone(),
                        required: *required,
                    };
                }
            }
        }
        let direction = direction_of_change(&eval_kind);
        let compat = if let Some(d) = direction {
            classify(&eval_kind, d)
        } else {
            classify(&eval_kind, Direction::Response)
        };
        let eval_change = crate::federation::contracts::diff::Change {
            service: change.service.clone(),
            kind: eval_kind,
        };
        let impact = evaluate(&eval_change, &base_surface, &head_surface, &coverage);
        evaluated.push((impact, compat, change.kind, change.service));
    }

    let min_class = min_impact.unwrap_or(Class::NoKnownImpact);
    let mut kept: Vec<Value> = Vec::new();
    let mut compatible_changes: u32 = 0;
    // A shared component (`$ref: Order`) fans one mutation out to
    // every endpoint that resolves it; `compatible_changes` counts
    // the *mutation* once (ground truth scenario 2: one optional
    // `currency` addition → 1, not 3).
    let mut compatible_seen: BTreeSet<String> = BTreeSet::new();
    for (impact, compat, kind, service) in evaluated {
        if let Some(ref sf) = service_filter {
            if service.0.as_str() != sf.as_str() {
                continue;
            }
        }
        if let Some(ref rf) = repo_filter {
            if !change_repo_set(&kind, &service, &base_index, &head_index).contains(rf.as_str()) {
                continue;
            }
        }
        let cls = impact.class;
        if matches!(cls, Class::NoKnownImpact) && matches!(compat, Compat::Compatible) {
            // §9.5: "Compatible → not reported; counted in
            // `compatible_changes`".
            if impact.compatible_changes > 0 && compatible_seen.insert(compatible_identity(&kind)) {
                compatible_changes += impact.compatible_changes;
            }
            continue;
        }
        if cls < min_class {
            continue;
        }
        let enclosing = match &kind {
            ChangeKind::ConsumerEndpointUnmatched { consumer }
            | ChangeKind::ConsumerFieldUnmatched { consumer, .. }
            | ChangeKind::ConsumerRebound { consumer, .. } => {
                enclosing_sender(head_fed.backend.as_ref(), &consumer.caller)
            }
            _ => None,
        };
        let mut value = impact_to_value(
            &impact,
            &kind,
            &service,
            compat,
            cap,
            &head_index,
            enclosing.as_ref(),
        );
        attach_change_paths(
            &mut value,
            &kind,
            &service,
            &base_index,
            &base_fed,
            &head_fed,
            cap,
            &head_label,
            started,
        )?;
        kept.push(value);
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

/// §12 sort: `(side, service, key, kind, direction, field)`.
/// Missing `direction` / `field` (endpoint-level changes) sort as
/// the empty string, which keeps the order stable and total.
fn sort_key(v: &Value) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}",
        v["side"].as_str().unwrap_or(""),
        v["endpoint"]["service"].as_str().unwrap_or(""),
        v["endpoint"]["key"].as_str().unwrap_or(""),
        v["kind"].as_str().unwrap_or(""),
        v["direction"].as_str().unwrap_or(""),
        v["field"].as_str().unwrap_or("")
    )
}

/// The logical identity of a change for `compatible_changes`
/// deduplication: endpoint-independent (a shared component's
/// mutation looks identical across the endpoints that resolve it).
fn compatible_identity(kind: &ChangeKind) -> String {
    match kind {
        ChangeKind::FieldRemoved {
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
        } => {
            format!(
                "{}|{}|{}",
                kind_label(kind),
                direction_label_str(*direction),
                path
            )
        }
        ChangeKind::EnumValueRemoved {
            direction,
            path,
            value,
            ..
        }
        | ChangeKind::EnumValueAdded {
            direction,
            path,
            value,
            ..
        } => format!(
            "{}|{}|{}|{}",
            kind_label(kind),
            direction_label_str(*direction),
            path,
            value
        ),
        ChangeKind::FieldRenamed {
            direction,
            from,
            to,
            ..
        } => format!(
            "FieldRenamed|{}|{}|{}",
            direction_label_str(*direction),
            from,
            to
        ),
        ChangeKind::PathChanged { from, to } | ChangeKind::MethodChanged { from, to } => {
            format!("{}|{}|{}", kind_label(kind), from, to)
        }
        ChangeKind::EndpointAdded { key } | ChangeKind::EndpointRemoved { key } => {
            format!("{}|{}", kind_label(kind), key)
        }
        other => format!("{:?}", other),
    }
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

/// The endpoint a provider-side change is about (§9.1 ids).
/// Consumer-side kinds carry no endpoint of their own here.
fn change_endpoint_id(kind: &ChangeKind, service: &ServiceName) -> Option<EndpointId> {
    match kind {
        ChangeKind::FieldRemoved { endpoint, .. }
        | ChangeKind::FieldAdded { endpoint, .. }
        | ChangeKind::FieldRenamed { endpoint, .. }
        | ChangeKind::FieldTypeChanged { endpoint, .. }
        | ChangeKind::RequirednessChanged { endpoint, .. }
        | ChangeKind::NullabilityChanged { endpoint, .. }
        | ChangeKind::EnumValueRemoved { endpoint, .. }
        | ChangeKind::EnumValueAdded { endpoint, .. }
        | ChangeKind::ChangedWithoutSchema { endpoint }
        | ChangeKind::HandlerChanged { endpoint } => Some(endpoint.clone()),
        ChangeKind::EndpointRemoved { key } | ChangeKind::EndpointAdded { key } => {
            Some((service.clone(), key.clone()))
        }
        ChangeKind::PathChanged { to, .. } | ChangeKind::MethodChanged { to, .. } => {
            Some((service.clone(), to.clone()))
        }
        ChangeKind::ConsumerEndpointUnmatched { .. }
        | ChangeKind::ConsumerFieldUnmatched { .. }
        | ChangeKind::ConsumerRebound { .. } => None,
    }
}

/// Every repo a change touches: the consumer's repo (consumer-side
/// kinds), the endpoint's providers' repos (either view), and the
/// service's declared repo. Used by `diff_contracts`' `repo`
/// filter — mirrors how `list_contracts` resolves `repo` against
/// provider `GlobalId`s.
fn change_repo_set(
    kind: &ChangeKind,
    service: &ServiceName,
    base_index: &ContractIndex,
    head_index: &ContractIndex,
) -> BTreeSet<String> {
    let mut repos: BTreeSet<String> = BTreeSet::new();
    match kind {
        ChangeKind::ConsumerEndpointUnmatched { consumer }
        | ChangeKind::ConsumerFieldUnmatched { consumer, .. }
        | ChangeKind::ConsumerRebound { consumer, .. } => {
            repos.insert(consumer.caller.repo.as_str().to_string());
        }
        _ => {}
    }
    if let Some(eid) = change_endpoint_id(kind, service) {
        for idx in [base_index, head_index] {
            if let Some(ep) = idx.endpoints.get(&eid) {
                for p in &ep.providers {
                    repos.insert(p.node_id.repo_id().to_string());
                }
            }
        }
    }
    for idx in [head_index, base_index] {
        if let Some(info) = idx.services.get(service) {
            repos.insert(info.repo.as_str().to_string());
        }
    }
    repos
}

/// Resolve the `Field` node's GlobalId for
/// `(endpoint, direction, json_path)` (§9.5: field-level traces
/// start at the changed `Field`). The schema node's `HasField`
/// edges (`Schema → Field`, §6.4) carry the direction; the
/// fallback scans `Field` nodes of the service's repo by name.
fn find_field_node(
    backend: &dyn GraphBackend,
    index: &ContractIndex,
    endpoint_id: &EndpointId,
    direction: Direction,
    json_path: &str,
) -> Option<String> {
    let endpoint = index.endpoints.get(endpoint_id)?;
    let schema = endpoint.schemas.get(&direction)?;
    // Walk the schema node's `HasField` edges (§6.4): the field
    // belongs to *this* endpoint's schema. When the endpoint has no
    // schema node (code-only route) or the field isn't in the
    // schema, there is no Field to trace from — §9.5 then starts
    // from the endpoint's provider nodes instead. A repo-wide name
    // scan would pick an arbitrary endpoint's field (the fixture's
    // `customer_id` appears in four schemas).
    let edges = backend.all_edges().ok()?;
    let mut targets: Vec<String> = edges
        .iter()
        .filter(|e| e.edge_type == crate::schema::EdgeType::HasField)
        .filter(|e| e.source_id == schema.node_id.as_str())
        .map(|e| e.target_id.clone())
        .collect();
    targets.sort();
    for target in &targets {
        if let Ok(Some(node)) = backend.get_node(target) {
            if node.name == json_path {
                return Some(target.clone());
            }
        }
    }
    None
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
    let mut line_ranges = std::collections::BTreeMap::new();
    for (repo, base_sha) in &base.repos {
        let head_sha = head
            .repos
            .get(repo)
            .cloned()
            .unwrap_or_else(|| base_sha.clone());
        // One pass over the git diff for both the changed paths and the
        // changed line spans — `diff_repo` + `diff_lines_repo` would
        // walk the tree twice.
        let (files, lines) = src.diff_repo_with_lines(repo, base_sha, &head_sha);
        by_repo.insert(repo.clone(), files);
        if let Some(ranges) = lines {
            line_ranges.insert(repo.clone(), ranges);
        }
    }
    MultiRepoChangedFiles {
        by_repo,
        line_ranges,
    }
}

async fn load_snapshot(
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
    let outcome = mgr.get(label, 1_000).await.map_err(|e| match e {
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

/// Load a snapshot's `ContractIndex` together with the resident
/// federation that backs it. The third element is the residency
/// `HoldGuard` — callers must keep it alive for the whole analysis
/// (§8.5), which is what makes the backend usable for §9.5 traces.
fn snapshot_contract_index(
    ctx: &McpContext<'_>,
    record: &SnapshotRecord,
    args_map: &Map<String, Value>,
    started: Instant,
) -> Result<
    (
        Arc<ContractIndex>,
        Arc<crate::federation::contracts::snapshots::SnapshotFederation>,
        crate::federation::contracts::snapshots::manager::HoldGuard,
    ),
    ToolOutcome,
> {
    let mgr = ctx.snapshots.ok_or_else(|| {
        error_outcome(
            "snapshot_manager_unavailable",
            "snapshot manager is not configured for this server",
            None,
            &record.id,
            started,
        )
    })?;
    // §10.5: analysis tools default 5_000 ms residency grace.
    let wait_ms = std::cmp::min(
        args_map
            .get("wait_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(5_000),
        60_000,
    );
    let (fed, guard) =
        mgr.from_snapshot_with_wait_ms(record, wait_ms)
            .map_err(|error| match error {
                crate::error::LainError::SnapshotResidencyBusy { retry_after_ms } => error_outcome(
                    "busy",
                    "snapshot residency busy",
                    Some(json!({"retry_after_ms": retry_after_ms})),
                    &record.id,
                    started,
                ),
                other => error_outcome(
                    "invalid_argument",
                    format!("from_snapshot: {other}"),
                    None,
                    &record.id,
                    started,
                ),
            })?;
    let ci = fed.contract_index.read().clone();
    let index = ci.ok_or_else(|| {
        error_outcome(
            "snapshot_not_ready",
            "snapshot has no contract index",
            Some(json!({"snapshot": record.id})),
            &record.id,
            started,
        )
    })?;
    Ok((index, fed, guard))
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

fn build_repo_coverage_with_ledgers(
    data_dir: &std::path::Path,
    head: &SnapshotRecord,
) -> (
    Vec<crate::federation::contracts::diff::RepoCoverage>,
    BTreeMap<String, crate::federation::contracts::coverage::RepoCoverage>,
) {
    let mut out = Vec::new();
    let mut ledgers = BTreeMap::new();
    for (repo, commit) in &head.repos {
        let key = crate::federation::contracts::index_cache::CacheKey::new(
            repo,
            commit,
            &head.analyzer_version,
        );
        let ledger_path = crate::federation::contracts::index_cache::entry_dir(data_dir, &key)
            .join(crate::federation::contracts::coverage::LEDGER_FILE);
        let ledger_read = crate::federation::contracts::coverage::read_ledger(&ledger_path);
        let loaded_cover = ledger_read
            .as_ref()
            .ok()
            .and_then(|l| l.as_ref())
            .and_then(|l| l.by_repo.get(repo).cloned());
        let ledger_error = match &ledger_read {
            Err(e) => Some(format!("coverage ledger unreadable: {e}")),
            Ok(None) => Some("coverage ledger missing; reindex required".to_string()),
            Ok(Some(ledger)) if !ledger.by_repo.contains_key(repo) => {
                Some("coverage ledger has no entry for repository".to_string())
            }
            Ok(Some(_)) => None,
        };

        let mut sensor_counts = BTreeMap::new();
        let mut languages = std::collections::BTreeSet::new();
        let mut skips = Vec::new();
        let mut unresolved = Vec::new();
        let mut analyzer_version = Some(head.analyzer_version.clone());

        if let Some(ref c) = loaded_cover {
            for (k, v) in &c.sensor_counts {
                sensor_counts.insert(k.clone(), *v as u32);
            }
            languages = c.languages_present.clone();
            analyzer_version = Some(c.cache_key.analyzer_version.clone());
            for bucket in c.ledger.values() {
                for s in bucket.values() {
                    skips.extend(s.files_skipped.clone());
                    unresolved.extend(s.unresolved.clone());
                }
            }
            ledgers.insert(repo.clone(), c.clone());
        } else {
            let cache = crate::federation::contracts::index_cache::IndexCache::new(data_dir);
            if let Ok(Some(m)) = cache.read_manifest(&key) {
                for (k, v) in &m.sensor_counts {
                    sensor_counts.insert(k.clone(), *v as u32);
                }
            }
            let missing = crate::federation::contracts::coverage::RepoCoverage {
                cache_key: key.clone(),
                error: ledger_error.clone(),
                ..Default::default()
            };
            ledgers.insert(repo.clone(), missing);
        }

        let mut entry = crate::federation::contracts::diff::RepoCoverage {
            repo: repo.clone(),
            commit: Some(commit.clone()),
            state: "indexed".to_string(),
            sensor_counts,
            error: ledger_error,
            languages,
            skips,
            unresolved,
            analyzer_version,
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
    (out, ledgers)
}

#[allow(dead_code)]
fn build_repo_coverage(
    head: &SnapshotRecord,
) -> Vec<crate::federation::contracts::diff::RepoCoverage> {
    let default_dir = std::path::PathBuf::from(".");
    let (repos, _) = build_repo_coverage_with_ledgers(&default_dir, head);
    repos
}

fn impact_to_value(
    impact: &Impact,
    kind: &ChangeKind,
    service: &ServiceName,
    compat: Compat,
    _cap: usize,
    head_index: &ContractIndex,
    enclosing_sender: Option<&(String, String)>,
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
    let endpoint = endpoint_from_change(kind, service, head_index);
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
    // Consumer-side changes name the consumer itself (§15.2
    // scenario 20 / ground truth).
    if let ChangeKind::ConsumerEndpointUnmatched { consumer }
    | ChangeKind::ConsumerFieldUnmatched { consumer, .. }
    | ChangeKind::ConsumerRebound { consumer, .. } = kind
    {
        // §15.2 scenario 20: `symbol` is the *enclosing function*
        // (the `SendsHttp` source), not the call node — the
        // consumer key's `caller.name` is the URL template.
        let (file, symbol) = enclosing_sender
            .map(|(p, n)| (p.as_str(), n.as_str()))
            .unwrap_or((consumer.caller.path.as_str(), consumer.caller.name.as_str()));
        value["consumer"] = json!({
            "service": service.0,
            "repo": consumer.caller.repo.as_str(),
            "file": file,
            "symbol": symbol,
        });
    }
    if let ChangeKind::ConsumerFieldUnmatched { field, .. } = kind {
        value["field"] = json!(field.to_string());
    }
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
    if let ChangeKind::ChangedWithoutSchema { endpoint } | ChangeKind::HandlerChanged { endpoint } =
        kind
    {
        if let Some(ep) = head_index.endpoints.get(endpoint) {
            let handlers: Vec<Value> = ep
                .providers
                .iter()
                .filter_map(|p| {
                    p.handler.as_ref().map(|h| {
                        json!({
                            // The provider node's repo, not `h.repo`:
                            // `RepoId::new` rejects paths containing
                            // `/`, so every sensor's `unwrap_or_else`
                            // fallback ran and minted the sensor name
                            // into `SymbolKey.repo`. Evidence must name
                            // a repository an external client can
                            // resolve.
                            "repo": p.node_id.repo_id(),
                            "file": h.path,
                            "symbol": h.name,
                        })
                    })
                })
                .collect();
            if !handlers.is_empty() {
                value["handlers"] = json!(handlers);
            }
        }
    }
    value
}

fn endpoint_from_change(
    kind: &ChangeKind,
    service: &ServiceName,
    head_index: &ContractIndex,
) -> Value {
    let (endpoint_service, key) = match kind {
        ChangeKind::EndpointRemoved { key } | ChangeKind::EndpointAdded { key } => {
            (service.clone(), key.clone())
        }
        ChangeKind::PathChanged { to, .. } | ChangeKind::MethodChanged { to, .. } => {
            (service.clone(), to.clone())
        }
        ChangeKind::FieldRemoved { endpoint, .. }
        | ChangeKind::FieldAdded { endpoint, .. }
        | ChangeKind::FieldRenamed { endpoint, .. }
        | ChangeKind::FieldTypeChanged { endpoint, .. }
        | ChangeKind::RequirednessChanged { endpoint, .. }
        | ChangeKind::NullabilityChanged { endpoint, .. }
        | ChangeKind::EnumValueRemoved { endpoint, .. }
        | ChangeKind::EnumValueAdded { endpoint, .. }
        | ChangeKind::ChangedWithoutSchema { endpoint }
        | ChangeKind::HandlerChanged { endpoint } => (endpoint.0.clone(), endpoint.1.clone()),
        ChangeKind::ConsumerEndpointUnmatched { consumer }
        | ChangeKind::ConsumerFieldUnmatched { consumer, .. }
        | ChangeKind::ConsumerRebound { consumer, .. } => {
            // §15.2 scenarios 3/20: a consumer-side change reports
            // the *provider's* endpoint — resolve the service that
            // provides the consumer's target key in the head view.
            // For unresolved consumers (`UrlExpr`), the consumer's
            // name carries `"<METHOD> <template>"`; surface the
            // could-match endpoint by template_matches (including
            // prefix-stripped, matching §7.4 + §9.7). This is what
            // scenario 3 needs to point at orders's
            // `/api/orders/{}` despite the consumer's
            // `/v1/api/orders/{}` template.
            match &consumer.target {
                crate::federation::contracts::diff::ConsumerTargetKey::Contract(k) => {
                    let resolved = head_index
                        .endpoints
                        .keys()
                        .find(|id| id.1 == *k)
                        .map(|id| id.0.clone())
                        .unwrap_or_else(|| service.clone());
                    (resolved, k.clone())
                }
                crate::federation::contracts::diff::ConsumerTargetKey::UrlExpr(name) => {
                    let (m, tmpl) = parse_method_template(name);
                    if let Some(t) = tmpl {
                        let mut best: Option<&EndpointId> = None;
                        for id in head_index.endpoints.keys() {
                            let (id_method, id_tmpl) = match &id.1 {
                                ContractKey::Http { method, template } => {
                                    (method.clone(), template.clone())
                                }
                                ContractKey::Topic { .. } => continue,
                                ContractKey::Rpc { .. } => continue,
                                ContractKey::Graphql { .. } => continue,
                                ContractKey::WebSocket { .. } => continue,
                                ContractKey::Table { .. } => continue,
                            };
                            if !method_compatible(&m, &id_method) {
                                continue;
                            }
                            if template_matches_with_prefix(&t, &id_tmpl) {
                                best = Some(id);
                                break;
                            }
                        }
                        if let Some(id) = best {
                            (id.0.clone(), id.1.clone())
                        } else {
                            (
                                service.clone(),
                                ContractKey::Http {
                                    method: m,
                                    template: t,
                                },
                            )
                        }
                    } else {
                        (
                            service.clone(),
                            ContractKey::Http {
                                method: m,
                                template: String::new(),
                            },
                        )
                    }
                }
            }
        }
    };
    json!({
        "service": endpoint_service.0,
        "key": key.to_string(),
    })
}

/// Parse `"<METHOD> <template>"` from the HttpClientCall name (the
/// shape `http_client_sensor` builds). `method = Unknown` and
/// `template = None` when the name is empty or doesn't match.
fn parse_method_template(name: &str) -> (MethodSpec, Option<String>) {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return (MethodSpec::Unknown, None);
    }
    let mut parts = trimmed.splitn(2, ' ');
    let verb = parts.next().unwrap_or("");
    let tmpl = parts.next().map(str::to_string);
    let method = match verb.to_ascii_uppercase().as_str() {
        "GET" => MethodSpec::Known(crate::federation::contracts::model::HttpMethod::Get),
        "POST" => MethodSpec::Known(crate::federation::contracts::model::HttpMethod::Post),
        "PUT" => MethodSpec::Known(crate::federation::contracts::model::HttpMethod::Put),
        "PATCH" => MethodSpec::Known(crate::federation::contracts::model::HttpMethod::Patch),
        "DELETE" => MethodSpec::Known(crate::federation::contracts::model::HttpMethod::Delete),
        "HEAD" => MethodSpec::Known(crate::federation::contracts::model::HttpMethod::Head),
        "OPTIONS" => MethodSpec::Known(crate::federation::contracts::model::HttpMethod::Options),
        "ANY" => MethodSpec::Known(crate::federation::contracts::model::HttpMethod::Any),
        _ => MethodSpec::Unknown,
    };
    (method, tmpl)
}

fn method_compatible(consumer: &MethodSpec, provider: &MethodSpec) -> bool {
    use crate::federation::contracts::model::HttpMethod;
    match (consumer, provider) {
        (MethodSpec::Unknown, _) => true,
        (MethodSpec::Known(_), MethodSpec::Unknown) => true,
        (MethodSpec::Known(a), MethodSpec::Known(b)) => *a == *b || matches!(*b, HttpMethod::Any),
    }
}

/// §7.4 template matching with prefix tolerance (up to 3 leading
/// literal segments). Mirrors `diff::template_matches` so an
/// unresolved consumer's `UrlExpr` name can find a candidate
/// endpoint for the consumer-side change payload.
fn template_matches_with_prefix(consumer: &str, provider: &str) -> bool {
    use crate::federation::contracts::route_match::match_route;
    let outcome = match_route(
        MethodSpec::Unknown,
        consumer,
        crate::federation::contracts::model::HttpMethod::Any,
        provider,
    );
    if outcome.is_match() {
        return true;
    }
    let segs: Vec<&str> = consumer.split('/').collect();
    for k in 1..=3 {
        if k + 1 > segs.len() - 1 {
            break;
        }
        let rest = segs[k + 1..].join("/");
        let stripped = format!("/{rest}");
        let outcome = match_route(
            MethodSpec::Unknown,
            &stripped,
            crate::federation::contracts::model::HttpMethod::Any,
            provider,
        );
        if outcome.is_match() {
            return true;
        }
    }
    false
}

/// Attach §9.5 impact `paths` to one change. Provider-side changes
/// trace in the **base** view (the old contract and its consumers);
/// consumer-side changes trace in the **head** view up from the
/// consumer's calling function. A field change starts at the
/// changed `Field`; an endpoint change starts at the endpoint's
/// provider nodes.
#[allow(clippy::too_many_arguments)]
fn attach_change_paths(
    value: &mut Value,
    kind: &ChangeKind,
    service: &ServiceName,
    base_index: &ContractIndex,
    base_fed: &crate::federation::contracts::snapshots::SnapshotFederation,
    head_fed: &crate::federation::contracts::snapshots::SnapshotFederation,
    cap: usize,
    label: &str,
    started: Instant,
) -> Result<(), ToolOutcome> {
    let consumer_side = matches!(
        kind,
        ChangeKind::ConsumerEndpointUnmatched { .. }
            | ChangeKind::ConsumerFieldUnmatched { .. }
            | ChangeKind::ConsumerRebound { .. }
    );
    let starts: Vec<String> = if consumer_side {
        let caller = match kind {
            ChangeKind::ConsumerEndpointUnmatched { consumer }
            | ChangeKind::ConsumerFieldUnmatched { consumer, .. }
            | ChangeKind::ConsumerRebound { consumer, .. } => &consumer.caller,
            _ => unreachable!(),
        };
        caller_node_id(head_fed.backend.as_ref(), caller)
            .map(|id| vec![id])
            .unwrap_or_default()
    } else {
        provider_trace_starts(kind, service, base_index, base_fed.backend.as_ref())
    };
    if starts.is_empty() {
        // Nothing to trace from (e.g. an endpoint the view does not
        // list): the wire keeps the empty `paths` array.
        return Ok(());
    }
    let backend = if consumer_side {
        &head_fed.backend
    } else {
        &base_fed.backend
    };
    let start_refs: Vec<&str> = starts.iter().map(String::as_str).collect();
    let outcome: ImpactResult =
        backend
            .traverse_impact(&start_refs, 10, cap, 0.0)
            .map_err(|e| {
                error_outcome(
                    "invalid_argument",
                    format!("traverse_impact failed: {e}"),
                    None,
                    label,
                    started,
                )
            })?;
    let mut paths = outcome.paths;
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
    let truncated = outcome.truncated || paths.len() > cap;
    if paths.len() > cap {
        paths.truncate(cap);
    }
    let path_values: Vec<Value> = paths.iter().map(graph_path_to_value).collect();
    value["paths"] = json!(path_values);
    value["truncated"] = json!(truncated);
    Ok(())
}

/// Start nodes for a provider-side change in the **base** view:
/// the changed `Field` when it exists (§9.5 "from the changed Field
/// if it exists in base"), otherwise the endpoint's provider nodes.
fn provider_trace_starts(
    kind: &ChangeKind,
    service: &ServiceName,
    index: &ContractIndex,
    backend: &dyn GraphBackend,
) -> Vec<String> {
    let endpoint = change_endpoint_id(kind, service);
    if let Some((direction, path)) = field_change_of(kind) {
        if let Some(eid) = endpoint.as_ref() {
            if let Some(fid) = find_field_node(backend, index, eid, direction, &path.to_string()) {
                return vec![fid];
            }
        }
    }
    if let Some(eid) = endpoint {
        if let Some(ep) = index.endpoints.get(&eid) {
            let ids: Vec<String> = ep
                .providers
                .iter()
                .map(|p| p.node_id.as_str().to_string())
                .collect();
            if !ids.is_empty() {
                return ids;
            }
        }
    }
    Vec::new()
}

/// `(direction, path)` for the field-bearing change kinds; the
/// rename traces from the `from` side, which is the field that
/// exists in base.
fn field_change_of(
    kind: &ChangeKind,
) -> Option<(Direction, crate::federation::contracts::model::JsonPath)> {
    match kind {
        ChangeKind::FieldRemoved {
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
        }
        | ChangeKind::EnumValueRemoved {
            direction, path, ..
        }
        | ChangeKind::EnumValueAdded {
            direction, path, ..
        } => Some((*direction, path.clone())),
        ChangeKind::FieldRenamed {
            direction, from, ..
        } => Some((*direction, from.clone())),
        _ => None,
    }
}

/// The GlobalId of a consumer's calling function in `backend`.
fn caller_node_id(
    backend: &dyn GraphBackend,
    caller: &crate::federation::contracts::model::SymbolKey,
) -> Option<String> {
    let nodes = backend.find_nodes_by_name(&caller.name).ok()?;
    nodes
        .into_iter()
        .filter(|n| {
            n.path == caller.path
                && GlobalId::parse(&n.id)
                    .map(|g| g.repo_id() == caller.repo.as_str())
                    .unwrap_or(false)
        })
        .map(|n| n.id)
        .min()
}

/// The wire's `consumer.file` / `consumer.symbol` (§15.2 scenario
/// 20: `symbol: build_invoice`, and §8 ground-truth `binds` list
/// `caller: build_invoice`). Preference order:
/// 1. the *reader* — the source of a `ReadsField` edge into a
///    `FieldRef` that `ReadsFrom` this call (the rule-5 caller that
///    consumes the response);
/// 2. the `SendsHttp` source (the sending function S).
fn enclosing_sender(
    backend: &dyn GraphBackend,
    caller: &crate::federation::contracts::model::SymbolKey,
) -> Option<(String, String)> {
    let nodes = backend.find_nodes_by_name(&caller.name).ok()?;
    let call_ids: Vec<String> = nodes
        .iter()
        .filter(|n| {
            n.node_type == crate::schema::NodeType::HttpClientCall
                && n.path == caller.path
                && GlobalId::parse(&n.id)
                    .map(|g| g.repo_id() == caller.repo.as_str())
                    .unwrap_or(false)
        })
        .map(|n| n.id.clone())
        .collect();
    if call_ids.is_empty() {
        return None;
    }
    let edges = backend.all_edges().ok()?;
    // FieldRefs that read from this call.
    let field_refs: Vec<String> = edges
        .iter()
        .filter(|e| e.edge_type == crate::schema::EdgeType::ReadsFrom)
        .filter(|e| call_ids.contains(&e.target_id))
        .map(|e| e.source_id.clone())
        .collect();
    let mut readers: Vec<String> = edges
        .iter()
        .filter(|e| e.edge_type == crate::schema::EdgeType::ReadsField)
        .filter(|e| field_refs.contains(&e.target_id))
        .map(|e| e.source_id.clone())
        .collect();
    readers.sort();
    let mut senders: Vec<String> = edges
        .iter()
        .filter(|e| e.edge_type == crate::schema::EdgeType::SendsHttp)
        .filter(|e| call_ids.contains(&e.target_id))
        .map(|e| e.source_id.clone())
        .collect();
    senders.sort();
    let sender = senders.first().cloned();
    // Candidate readers: functions other than the sending function
    // (the ground truth's consumer is the rule-5 caller that reads
    // the response, not S), never a File fallback node; then any
    // reader; then S.
    let non_sender: Vec<&String> = readers
        .iter()
        .filter(|r| Some(*r) != sender.as_ref())
        .collect();
    let resolve = |ids: &[String]| -> Option<(String, String)> {
        for id in ids {
            if let Ok(Some(node)) = backend.get_node(id) {
                if node.node_type != crate::schema::NodeType::File {
                    return Some((node.path.clone(), node.name.clone()));
                }
            }
        }
        None
    };
    if let Some(found) = resolve(&non_sender.iter().map(|s| (*s).clone()).collect::<Vec<_>>()) {
        return Some(found);
    }
    if let Some(found) = resolve(&readers) {
        return Some(found);
    }
    let src = sender?;
    let node = backend.get_node(&src).ok().flatten()?;
    Some((node.path.clone(), node.name.clone()))
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
        ChangeKind::HandlerChanged { .. } => "HandlerChanged",
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
    let mut reviewed: Vec<Value> = Vec::new();
    let mut unreviewed: Vec<Value> = Vec::new();
    let mut excluded: Vec<String> = Vec::new();
    let mut failed: Vec<Value> = Vec::new();
    for r in &s.reviewed {
        reviewed.push(json!({
            "repo": r.repo,
            "commit": r.commit,
            "dirty": r.dirty,
        }));
    }
    for u in &s.unreviewed {
        unreviewed.push(json!({
            "repo": u.repo,
            "reason": u.reason,
            "error": u.error,
        }));
        if u.reason == "excluded" {
            excluded.push(u.repo.clone());
        } else if u.reason == "failed" {
            failed.push(json!({
                "repo": u.repo,
                "error": u.error,
            }));
        }
    }
    json!({
        "reviewed": reviewed,
        "unreviewed": unreviewed,
        "excluded": excluded.clone(),
        "failed": failed.clone(),
        "configured_only": s.configured_only,
        "caveats": {
            "unconfigured_scope": "Only explicitly configured repositories were reviewed; unconfigured organization repositories are not visible to analysis.",
            "excluded_repositories": excluded,
            "failed_repositories": failed,
        }
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
                "languages": r.languages,
                "skips": r.skips,
                "unresolved": r.unresolved,
                "analyzer_version": r.analyzer_version,
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
        let display_cls = if cls == "NoKnownImpact" {
            "no known impact in reviewed scope"
        } else {
            cls
        };
        out.push_str(&format!(
            "- {} {} ({} {}) → {}\n",
            endpoint["service"].as_str().unwrap_or(""),
            endpoint["key"].as_str().unwrap_or(""),
            kind,
            compat,
            display_cls,
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

    let resolved = resolve_view(ctx, &args_map, started).await?;
    let Some(view_index) = resolved.index().cloned() else {
        return Err(error_outcome(
            "contract_not_found",
            "no contract index available for this snapshot",
            Some(json!({"endpoint": null})),
            &snap_label,
            started,
        ));
    };

    // The traversal backend doubles as the view the field arm
    // resolves `Field` node ids from (§9.5 traces field changes
    // from the changed `Field`).
    let backend = resolved.backend();

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
        let endpoint_id: EndpointId = (ServiceName(endpoint.0.clone()), key);
        let endpoint = view_index.endpoints.get(&endpoint_id).ok_or_else(|| {
            error_outcome(
                "contract_not_found",
                "endpoint is not present in the selected view",
                Some(json!({
                    "endpoint": {
                        "service": endpoint_id.0 .0,
                        "key": endpoint_id.1.to_string(),
                    }
                })),
                &snap_label,
                started,
            )
        })?;
        starts.extend(
            endpoint
                .providers
                .iter()
                .map(|provider| provider.node_id.as_str().to_string()),
        );
        // `ResponseSchema` / `HasField` are `Propagation::Incoming`
        // (`graph_backend.rs::impact_propagation`): the BFS walks
        // schema → endpoint and field → schema, so from the endpoint
        // node alone the endpoint's own `Schema` / `Field` nodes are
        // unreachable — an endpoint seed could only ever see "who
        // calls this", never "what this exposes". Seed the endpoint's
        // attached Schema nodes and their `HasField` targets (the
        // fields must be seeds too: `Incoming` registers `HasField`
        // under the Field node, so a Schema seed cannot reach its own
        // fields) alongside the endpoint's providers.
        let schema_ids: Vec<String> = endpoint
            .schemas
            .values()
            .map(|schema| schema.node_id.as_str().to_string())
            .collect();
        starts.extend(schema_ids.iter().cloned());
        // Propagate rather than silently skip: a skipped seed reverts
        // the trace to the pre-fix blindness where an endpoint cannot
        // see its own schema, and the response would look complete.
        // The standing rule is never claim absent when unanalysed —
        // silently degrading the seed set is the same failure.
        let edges = backend.all_edges().map_err(|e| ToolOutcome {
            structured: json!({}),
            text: format!("could not enumerate edges for the schema seed: {e}"),
            is_error: true,
        })?;
        {
            let mut field_ids: Vec<String> = edges
                .iter()
                .filter(|e| e.edge_type == crate::schema::EdgeType::HasField)
                .filter(|e| schema_ids.contains(&e.source_id))
                .map(|e| e.target_id.clone())
                .collect();
            field_ids.sort();
            starts.extend(field_ids);
        }
    } else if has_field {
        let field = from.get("field").cloned().unwrap_or(Value::Null);
        let endpoint = field.get("endpoint").cloned().unwrap_or(Value::Null);
        let endpoint_pair = parse_endpoint_for_trace(&endpoint)?;
        let endpoint_id = (
            ServiceName(endpoint_pair.0.clone()),
            ContractKey::from_str(&endpoint_pair.1).map_err(|e| {
                error_outcome(
                    "invalid_argument",
                    format!("malformed field.endpoint.key: {e}"),
                    Some(json!({"arg": "from.field.endpoint.key"})),
                    &snap_label,
                    started,
                )
            })?,
        );
        let direction_raw = field
            .get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or("response");
        let direction = match direction_raw {
            "request" => Direction::Request,
            "response" => Direction::Response,
            "payload" => Direction::Payload,
            other => {
                return Err(error_outcome(
                    "invalid_argument",
                    format!("invalid field.direction: {other:?}"),
                    Some(json!({"arg": "from.field.direction"})),
                    &snap_label,
                    started,
                ));
            }
        };
        let json_path = field
            .get("json_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let field_id = find_field_node(
            backend.as_ref(),
            &view_index,
            &endpoint_id,
            direction,
            json_path,
        )
        .ok_or_else(|| {
            error_outcome(
                "contract_not_found",
                format!("field {json_path:?} not found on endpoint"),
                Some(json!({
                    "endpoint": {
                        "service": endpoint_pair.0,
                        "key": endpoint_pair.1,
                    },
                    "direction": direction_raw,
                    "json_path": json_path,
                })),
                &snap_label,
                started,
            )
        })?;
        starts.push(field_id);
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
    let scope = resolved.scope().clone();
    let data = json!({
        "paths": paths_value,
        "truncated": truncated,
        "scope": scope,
    });
    let envelope = success_envelope_with_view(
        data.clone(),
        resolved.label(),
        resolved.reproducible(),
        resolved.view_info(),
        started,
    );
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
    let start = p
        .hops
        .first()
        .map(|hop| {
            if hop.edge.target_id == hop.node.id && hop.edge.source_id != hop.node.id {
                hop.edge.source_id.as_str()
            } else if hop.edge.source_id == hop.node.id && hop.edge.target_id != hop.node.id {
                hop.edge.target_id.as_str()
            } else {
                hop.node.id.as_str()
            }
        })
        .unwrap_or("");
    json!({
        "start": start,
        "min_confidence": p.min_confidence,
        "hops": hops,
    })
}

fn provenance_label(p: Option<&EdgeProvenance>) -> Value {
    let Some(p) = p else {
        // A legacy edge with no recorded provenance. Treated as 0.0
        // here and in `traverse_impact`, because confidence must be
        // evidence-backed. Sensors now emit `Static` by default
        // (`GraphEdge::new`), so this arm only fires for edges
        // deserialized from an older store.
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
    let resolved = resolve_view(ctx, &args_map, started).await?;
    let Some(view) = resolved.index().cloned() else {
        let data = json!({
            "complete": true,
            "scope": resolved.scope().clone(),
            "repos": [],
        });
        let envelope = success_envelope(data.clone(), &snap_label, snap_label != "live", started);
        return Ok(outcome(envelope, &data, render_coverage(&data)));
    };
    let endpoint_arg = args_map.get("endpoint").cloned();
    let scope_value = resolved.scope().clone();
    let scope = parse_scope(scope_value);
    let (coverage_repos, repo_coverages) = if snap_label != "live" {
        if let Some(mgr) = ctx.snapshots {
            if let Ok(outcome) = mgr.get(&snap_label, 5_000).await {
                build_repo_coverage_with_ledgers(mgr.data_dir(), &outcome.record)
            } else {
                (Vec::new(), BTreeMap::new())
            }
        } else {
            (Vec::new(), BTreeMap::new())
        }
    } else {
        let mut repos = Vec::new();
        let mut coverages = BTreeMap::new();
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
                let mut sensor_counts: BTreeMap<String, u32> = BTreeMap::new();
                if let Some(r) = fed.get_repo(&id) {
                    let all_nodes = r.db().get_all_nodes();
                    let mut http_routes = 0u32;
                    let mut tables = 0u32;
                    for n in all_nodes {
                        match n.node_type {
                            crate::schema::NodeType::HttpRoute => http_routes += 1,
                            crate::schema::NodeType::Table => tables += 1,
                            _ => {}
                        }
                    }
                    if http_routes > 0 {
                        sensor_counts.insert("http_routes".into(), http_routes);
                    }
                    if tables > 0 {
                        sensor_counts.insert("sql_tables".into(), tables);
                    }
                }
                let live_coverage_error =
                    "coverage ledger unavailable for live view; prepare a pinned snapshot"
                        .to_string();
                let cover = crate::federation::contracts::coverage::RepoCoverage {
                    cache_key: crate::federation::contracts::index_cache::CacheKey::new(
                        id.as_str(),
                        commit.as_deref().unwrap_or(""),
                        crate::federation::contracts::analyzer_version(),
                    ),
                    sensor_counts: sensor_counts
                        .iter()
                        .map(|(k, v)| (k.clone(), *v as u64))
                        .collect(),
                    error: Some(live_coverage_error.clone()),
                    ..Default::default()
                };
                coverages.insert(id.as_str().to_string(), cover);
                repos.push(crate::federation::contracts::diff::RepoCoverage {
                    repo: id.as_str().to_string(),
                    commit,
                    state: state_str.to_string(),
                    sensor_counts,
                    error: Some(live_coverage_error),
                    languages: std::collections::BTreeSet::new(),
                    skips: Vec::new(),
                    unresolved: Vec::new(),
                    analyzer_version: Some(crate::federation::contracts::analyzer_version()),
                });
            }
        }
        (repos, coverages)
    };

    let mut coverage = build_coverage(&view, coverage_repos, scope);
    coverage.repo_coverages = repo_coverages;

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
    let complete = coverage_complete(&coverage, endpoint_id.as_ref(), &view);
    coverage.complete = complete;
    let mut value = coverage_to_value(&coverage);
    value["complete"] = json!(complete);
    let envelope = success_envelope_with_view(
        value.clone(),
        resolved.label(),
        resolved.reproducible(),
        resolved.view_info(),
        started,
    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::contracts::diff::ConsumerKey;
    use crate::federation::contracts::diff::{
        Affected, Reason as DiffReason, ReviewedRepo, UnreviewedRepo,
    };
    use crate::federation::contracts::model::{JsonPath, SymbolKey};
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
            &ContractIndex::default(),
            None,
        );
        assert_eq!(value["side"], "consumer");
        assert_eq!(value["compat"], "Breaking");
        assert_eq!(value["impact"]["class"], "NeedsInvestigation");
    }

    /// `handlers[].repo` must be the repo that owns the provider node,
    /// never the sensor name that leaked into `SymbolKey.repo`.
    ///
    /// `RepoId::new` rejects any value containing `/`, and every real
    /// sensor call passes `root.to_string_lossy()` (a filesystem path),
    /// so the `unwrap_or_else` fallback always ran and minted
    /// `RepoId::new("http-sensor")`. That value is rendered as evidence
    /// and an external client cannot resolve it as a repository.
    #[test]
    fn handler_repo_is_the_provider_repo_not_a_sensor_name() {
        let provider_id =
            GlobalId::parse("orders:HttpRoute:src/orders/label.rs:GET /api/orders/%3Aid/label:8")
                .expect("valid global id");
        let ep_id: crate::federation::contracts::index::EndpointId = (
            ServiceName("orders".into()),
            ContractKey::Http {
                method: MethodSpec::Known(crate::federation::contracts::model::HttpMethod::Get),
                template: "/api/orders/{}/label".into(),
            },
        );
        let endpoint = crate::federation::contracts::index::Endpoint {
            id: ep_id.clone(),
            method: crate::federation::contracts::model::HttpMethod::Get,
            template: "/api/orders/{}/label".into(),
            providers: vec![crate::federation::contracts::index::EndpointProvider {
                node_id: provider_id,
                origin: crate::federation::contracts::model::ProviderOrigin::Code,
                handler: Some(SymbolKey {
                    repo: RepoId::new("http-sensor").unwrap(),
                    path: "src/orders/label.rs".into(),
                    container: None,
                    name: "get_order_label".into(),
                }),
                operation_id: None,
            }],
            schemas: Default::default(),
        };
        let mut head_index = ContractIndex::default();
        head_index.endpoints.insert(ep_id.clone(), endpoint);

        let kind = ChangeKind::ChangedWithoutSchema {
            endpoint: ep_id.clone(),
        };
        let impact = Impact {
            service: ServiceName("orders".into()),
            kind: kind.clone(),
            class: Class::NeedsInvestigation,
            reason: Some(DiffReason::NeedsReview),
            affected: vec![],
            scope: DiffScope::default(),
            coverage: DiffCoverage::default(),
            compatible_changes: 0,
        };
        let value = impact_to_value(
            &impact,
            &kind,
            &ServiceName("orders".into()),
            Compat::NeedsReview,
            50,
            &head_index,
            None,
        );

        let handlers = value["handlers"].as_array().expect("handlers present");
        assert_eq!(handlers[0]["symbol"], "get_order_label");
        assert_eq!(handlers[0]["file"], "src/orders/label.rs");
        assert_eq!(
            handlers[0]["repo"], "orders",
            "handlers[].repo must be the provider's repo, not the sensor name"
        );
        assert_ne!(handlers[0]["repo"], "http-sensor");
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
        // A sensor-emitted edge with no finer attribution is still a
        // static fact — never `unknown`/0.0, which would make
        // `min_confidence` useless as a filter.
        assert_eq!(v["hops"][0]["provenance"]["kind"], "static");
        assert_eq!(v["hops"][0]["provenance"]["confidence"], 1.0);
    }
}
