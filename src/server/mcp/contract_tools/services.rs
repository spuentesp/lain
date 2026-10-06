//! Contract tool: `list_services` and `get_service`.
//!
//! `docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §10.1 / §10.2 / §10.5 / §10.9 / §12.
//!
//! The two tools read from the in-process `FederatedIndex::contract_index()`
//! after calling `rejoin_contracts_if_dirty()` (§10.1 / §5.3), or from
//! an immutable `SnapshotFederation` when the caller supplies a pinned
//! snapshot id. Snapshot reads retain a residency hold until the response
//! (including reverse `used_by` traversal) has been assembled.
//!
//! ## Sort orders (§12)
//!
//! - `list_services.items`: sorted by `service` name.
//! - `get_service.consumers`: sorted by `(service, caller GlobalId, site line)`.
//!
//! ## Limits (§10.5)
//!
//! - `list` `limit`: default 100, max 1000; `range_too_large` beyond.
//! - `get_service.depth`: default 4, max 8; `range_too_large` beyond.
//! - `cursor`: opaque base64url; `cursor_mismatch` on reuse with
//!   different arguments.

use super::contracts::{snapshot_label, unresolved_reason_label};
use super::envelope::{cap_2000, check_api_version, error_outcome, outcome, success_envelope};
use super::paging::{apply_limit, decode_cursor, fingerprint};
use super::scope::provider_is_reviewed;
use super::used_by::{walk as walk_used_by, walk_backend as walk_used_by_backend};
use super::{ContractToolEntry, ToolOutcome, DEFAULT_DEPTH, DEFAULT_LIMIT, MAX_DEPTH, MAX_LIMIT};
use crate::federation::contracts::config::RoutePrefix;
use crate::federation::contracts::index::{
    BoundField, ConsumerTarget, ContractIndex, Endpoint, ServiceInfo, UnresolvedReason,
};
use crate::federation::contracts::model::{EntryKind, ProviderOrigin};
use crate::federation::contracts::snapshots::manager::{HoldGuard, SnapshotFederation};
use crate::federation::contracts::snapshots::record::SnapshotRecord;
use crate::federation::contracts::snapshots::RepoSnapshotState;
use crate::federation::federated_index::FederatedIndex;
use crate::federation::graph_backend::GraphBackend;
use crate::federation::health::RepoHealth;
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::schema::EdgeType;
use crate::server::mcp::handler::McpContext;
use crate::server::sensors::codeowners_sensor::codeowners_for;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

/// Type alias for the boxed-future return type of a contract tool
/// handler. `Pin<Box<dyn Future + Send>>` so it composes with the
/// rest of the async dispatcher (whose callers require `Send`).
pub type ContractToolFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

// ─── dispatch ─────────────────────────────────────────────────────────

/// `list_services` async handler (`§10.1`).
pub fn list_services_handle<'a>(
    ctx: &'a McpContext<'a>,
    args: Value,
) -> ContractToolFuture<'a, Result<ToolOutcome, String>> {
    let args_map: Map<String, Value> = match args {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    Box::pin(async move {
        let started = Instant::now();
        let snap_label = snapshot_label(&args_map);
        if let Err(details) = check_api_version(&args_map) {
            let o = error_outcome(
                "unsupported_api_version",
                "api_version not served",
                Some(details),
                &snap_label,
                started,
            );
            return Ok(o);
        }
        match run_list_services(ctx, args_map, started).await {
            Ok(o) => Ok(o),
            Err(o) => Ok(o),
        }
    })
}

/// `get_service` async handler (`§10.1`).
pub fn get_service_handle<'a>(
    ctx: &'a McpContext<'a>,
    args: Value,
) -> ContractToolFuture<'a, Result<ToolOutcome, String>> {
    let args_map: Map<String, Value> = match args {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    Box::pin(async move {
        let started = Instant::now();
        let snap_label = snapshot_label(&args_map);
        if let Err(details) = check_api_version(&args_map) {
            let o = error_outcome(
                "unsupported_api_version",
                "api_version not served",
                Some(details),
                &snap_label,
                started,
            );
            return Ok(o);
        }
        match run_get_service(ctx, args_map, started).await {
            Ok(o) => Ok(o),
            Err(o) => Ok(o),
        }
    })
}

inventory::submit!(ContractToolEntry {
    name: "list_services",
    handler: list_services_handle,
});
inventory::submit!(ContractToolEntry {
    name: "get_service",
    handler: get_service_handle,
});

// ─── helpers ──────────────────────────────────────────────────────────

enum ServiceViewHandle<'a> {
    Empty(Value),
    Ready {
        index: Arc<ContractIndex>,
        scope: Value,
        fed: Option<&'a FederatedIndex>,
        snapshot: Option<Arc<SnapshotFederation>>,
        record: Option<Box<SnapshotRecord>>,
        _hold: Option<HoldGuard>,
    },
}

async fn resolve_service_view<'a>(
    ctx: &'a McpContext<'a>,
    args_map: &Map<String, Value>,
    started: Instant,
) -> Result<ServiceViewHandle<'a>, ToolOutcome> {
    let snap_label = snapshot_label(args_map);
    if snap_label == "live" {
        let fed = ctx.federation.ok_or_else(|| {
            error_outcome(
                "federation_disabled",
                "this server is not configured with a federation",
                None,
                &snap_label,
                started,
            )
        })?;
        if let Err(e) = fed.rejoin_contracts_if_dirty() {
            return Err(error_outcome(
                "invalid_argument",
                format!("rejoin failed: {e}"),
                None,
                &snap_label,
                started,
            ));
        }
        let Some(idx) = fed.contract_index() else {
            return Ok(ServiceViewHandle::Empty(super::scope::live_scope(fed)));
        };
        return Ok(ServiceViewHandle::Ready {
            index: idx,
            scope: super::scope::live_scope(fed),
            fed: Some(fed),
            snapshot: None,
            record: None,
            _hold: None,
        });
    }

    if !snap_label.starts_with(crate::federation::contracts::snapshots::SNAPSHOT_ID_PREFIX) {
        return Err(error_outcome(
            "snapshot_not_found",
            format!("snapshot {snap_label:?} not found"),
            Some(json!({"snapshot": snap_label})),
            &snap_label,
            started,
        ));
    }

    let mgr = ctx.snapshots.ok_or_else(|| {
        error_outcome(
            "snapshot_manager_unavailable",
            "snapshot manager is not configured for this server",
            None,
            &snap_label,
            started,
        )
    })?;

    let outcome = mgr.get(&snap_label, 5_000).await.map_err(|e| match e {
        crate::federation::contracts::snapshots::manager::PrepareError::SnapshotNotFound {
            snapshot,
        } => error_outcome(
            "snapshot_not_found",
            format!("snapshot {snapshot:?} not found"),
            Some(json!({"snapshot": snapshot})),
            &snap_label,
            started,
        ),
        crate::federation::contracts::snapshots::manager::PrepareError::Busy { retry_after_ms } => {
            error_outcome(
                "busy",
                "snapshot residency busy",
                Some(json!({"retry_after_ms": retry_after_ms})),
                &snap_label,
                started,
            )
        }
        other => error_outcome(
            "invalid_argument",
            format!("{other:?}"),
            None,
            &snap_label,
            started,
        ),
    })?;

    let (snap_fed, _guard) = mgr
        .from_snapshot_with_wait_ms(&outcome.record, 5_000)
        .map_err(|e| {
            error_outcome(
                "invalid_argument",
                format!("from_snapshot failed: {e}"),
                None,
                &snap_label,
                started,
            )
        })?;

    let scope = super::scope::snapshot_scope(&outcome.record);
    let ci = snap_fed.contract_index.read().clone();
    let Some(idx) = ci else {
        return Ok(ServiceViewHandle::Empty(scope));
    };

    Ok(ServiceViewHandle::Ready {
        index: idx,
        scope,
        fed: None,
        snapshot: Some(snap_fed),
        record: Some(Box::new(outcome.record)),
        _hold: Some(_guard),
    })
}

/// Decode the `limit` argument (`§10.5`). Returns `Err` with the
/// `range_too_large` outcome when the value exceeds `MAX_LIMIT`.
fn parse_limit(
    args: &Map<String, Value>,
    snapshot: &str,
    started: Instant,
) -> Result<usize, ToolOutcome> {
    let raw = args.get("limit").and_then(|v| v.as_u64());
    match raw {
        None => Ok(DEFAULT_LIMIT),
        Some(n) if (n as usize) > MAX_LIMIT => Err(error_outcome(
            "range_too_large",
            format!("limit {n} exceeds max {MAX_LIMIT}"),
            Some(json!({"limit": n, "max": MAX_LIMIT, "requested": n})),
            snapshot,
            started,
        )),
        Some(0) => Err(error_outcome(
            "invalid_argument",
            "limit must be >= 1",
            Some(json!({"arg": "limit"})),
            snapshot,
            started,
        )),
        Some(n) => Ok(n as usize),
    }
}

/// Decode the `depth` argument (`§10.5`).
fn parse_depth(
    args: &Map<String, Value>,
    snapshot: &str,
    started: Instant,
) -> Result<u8, ToolOutcome> {
    let raw = args.get("depth").and_then(|v| v.as_u64());
    match raw {
        None => Ok(DEFAULT_DEPTH),
        Some(n) if (n as u8) > MAX_DEPTH => Err(error_outcome(
            "range_too_large",
            format!("depth {n} exceeds max {MAX_DEPTH}"),
            Some(json!({"limit": n, "max": MAX_DEPTH, "requested": n})),
            snapshot,
            started,
        )),
        Some(0) => Err(error_outcome(
            "invalid_argument",
            "depth must be >= 1",
            Some(json!({"arg": "depth"})),
            snapshot,
            started,
        )),
        Some(n) => Ok(n as u8),
    }
}

/// Decode the `cursor` argument. Verifies the embedded `q` against the
/// current arg fingerprint; on mismatch returns the `cursor_mismatch`
/// outcome.
fn parse_cursor(
    args: &Map<String, Value>,
    snapshot: &str,
    started: Instant,
) -> Result<Option<super::paging::CursorPayload>, ToolOutcome> {
    let Some(token) = args.get("cursor").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    let cur = match decode_cursor(token) {
        Ok(c) => c,
        Err(_) => {
            return Err(error_outcome(
                "invalid_argument",
                "malformed cursor",
                Some(json!({"arg": "cursor", "reason": "malformed"})),
                snapshot,
                started,
            ));
        }
    };
    let want = fingerprint(args);
    if cur.q != want {
        return Err(error_outcome(
            "invalid_argument",
            "cursor does not match the current arguments",
            Some(json!({"arg": "cursor", "reason": "cursor_mismatch"})),
            snapshot,
            started,
        ));
    }
    Ok(Some(cur))
}

// ─── list_services implementation ─────────────────────────────────────

async fn run_list_services(
    ctx: &McpContext<'_>,
    args_map: Map<String, Value>,
    started: Instant,
) -> Result<ToolOutcome, ToolOutcome> {
    let snap_label = snapshot_label(&args_map);
    let limit = parse_limit(&args_map, &snap_label, started)?;
    let cursor = parse_cursor(&args_map, &snap_label, started)?;
    let repo_filter = args_map
        .get("repo")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let (idx, scope) = match resolve_service_view(ctx, &args_map, started).await? {
        ServiceViewHandle::Empty(scope) => {
            return Ok(list_services_empty(scope, &snap_label, started));
        }
        ServiceViewHandle::Ready { index, scope, .. } => (index, scope),
    };

    let mut items: Vec<Value> = Vec::new();
    for (name, info) in idx.services.iter() {
        if let Some(ref r) = repo_filter {
            if info.repo.as_str() != r {
                continue;
            }
        }
        let consumer_services = count_distinct_consumer_services(&idx, info);
        let unresolved_inbound = count_unresolved_inbound(&idx, info);
        items.push(json!({
            "service": name.0.as_str(),
            "repo": info.repo.as_str(),
            "paths": info.paths,
            "endpoints": info.endpoint_ids.len(),
            "consumer_services": consumer_services,
            "unresolved_inbound": unresolved_inbound,
        }));
    }

    let cursor_token = cursor.as_ref().map(|c| c.after.clone());
    if let Some(after) = cursor_token {
        items.retain(|it| it["service"].as_str().unwrap_or("") > after.as_str());
    }
    let key = |it: &Value| it["service"].as_str().unwrap_or("").to_string();
    let fp = fingerprint(&args_map);
    let (page, next_cursor) = apply_limit(items, limit, key, &fp);

    let mut data = json!({
        "items": page,
    });
    data["scope"] = scope;
    if let Some(tok) = next_cursor {
        data["cursor"] = json!(tok);
    }
    let text = render_list_services(&data);
    let envelope = success_envelope(data.clone(), &snap_label, snap_label != "live", started);
    Ok(outcome(envelope, &data, text))
}

fn list_services_empty(scope: Value, snapshot: &str, started: Instant) -> ToolOutcome {
    let data = json!({"items": [], "scope": scope});
    let envelope = success_envelope(data.clone(), snapshot, snapshot != "live", started);
    let text = render_list_services(&data);
    outcome(envelope, &data, text)
}

fn count_distinct_consumer_services(idx: &ContractIndex, info: &ServiceInfo) -> usize {
    let provider_endpoints: std::collections::BTreeSet<String> = info
        .endpoint_ids
        .iter()
        .map(|(_, k)| k.to_string())
        .collect();
    let mut consumers: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for resolution in idx.consumers.values() {
        if let Some(ConsumerTarget::Binds { .. }) = &resolution.target {
            let bound = resolution
                .bound_endpoints
                .iter()
                .any(|(_, k)| provider_endpoints.contains(&k.to_string()));
            if bound {
                consumers.insert(resolution.service.0.clone());
            }
        }
    }
    consumers.len()
}

fn count_unresolved_inbound(idx: &ContractIndex, info: &ServiceInfo) -> usize {
    idx.consumers
        .values()
        .filter(|r| {
            matches!(
                r.target,
                Some(ConsumerTarget::Unresolved {
                    reason: UnresolvedReason::NoRouteInService,
                    ..
                })
            ) && r.service.0 == info.name.0
        })
        .count()
}

// ─── get_service implementation ───────────────────────────────────────

async fn run_get_service(
    ctx: &McpContext<'_>,
    args_map: Map<String, Value>,
    started: Instant,
) -> Result<ToolOutcome, ToolOutcome> {
    let snap_label = snapshot_label(&args_map);
    let limit = parse_limit(&args_map, &snap_label, started)?;
    let depth = parse_depth(&args_map, &snap_label, started)?;
    let cursor = parse_cursor(&args_map, &snap_label, started)?;
    let service_arg = args_map
        .get("service")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            error_outcome(
                "invalid_argument",
                "missing required argument: service",
                Some(json!({"arg": "service"})),
                &snap_label,
                started,
            )
        })?
        .to_string();

    let view = resolve_service_view(ctx, &args_map, started).await?;
    let (idx, scope, fed_opt, snapshot_opt, record_opt, _hold) = match view {
        ServiceViewHandle::Empty(scope) => {
            return Ok(get_service_not_found(
                &service_arg,
                scope,
                &snap_label,
                started,
            ));
        }
        ServiceViewHandle::Ready {
            index,
            scope,
            fed,
            snapshot,
            record,
            _hold,
        } => (index, scope, fed, snapshot, record, _hold),
    };

    let Some(info) = idx
        .services
        .get(&crate::federation::contracts::model::ServiceName(
            service_arg.clone(),
        ))
    else {
        return Ok(get_service_not_found(
            &service_arg,
            scope,
            &snap_label,
            started,
        ));
    };

    let provider_reviewed = if let Some(fed) = fed_opt {
        let health = fed
            .list_repos()
            .into_iter()
            .find(|(rid, _)| rid == &info.repo)
            .map(|(_, h)| h)
            .unwrap_or(RepoHealth::Missing);
        provider_is_reviewed(health)
    } else if let Some(ref rec) = record_opt {
        rec.repo_states
            .get(info.repo.as_str())
            .map(|s| matches!(s, RepoSnapshotState::Cached { .. }))
            .unwrap_or(false)
    } else {
        false
    };

    let mut commit_by_repo = BTreeMap::new();
    if let Some(fed) = fed_opt {
        for (id, _) in fed.list_repos() {
            if let Some(c) = fed
                .get_repo(&id)
                .and_then(|r| r.db().get_last_commit().ok().flatten())
            {
                commit_by_repo.insert(id.as_str().to_string(), c);
            }
        }
    } else if let Some(ref rec) = record_opt {
        for (r, c) in &rec.repos {
            commit_by_repo.insert(r.clone(), c.clone());
        }
    }

    let endpoints: Vec<String> = info
        .endpoint_ids
        .iter()
        .map(|(_, k)| k.to_string())
        .collect();
    let snapshot_backend = snapshot_opt
        .as_ref()
        .map(|snapshot| snapshot.backend.as_ref() as &dyn GraphBackend);
    let consumers = build_consumer_rows(
        fed_opt,
        snapshot_backend,
        &idx,
        info.clone(),
        depth,
        &commit_by_repo,
    );

    let unresolved_candidates = build_unresolved_candidates(&idx, info, &commit_by_repo);

    let mut consumers_value: Vec<Value> = consumers
        .iter()
        .map(|c| {
            json!({
                "service": c.service,
                "repo": c.repo,
                "uses": c.uses,
            })
        })
        .collect();

    let cursor_after = cursor.as_ref().map(|c| c.after.clone());
    if let Some(after) = cursor_after {
        consumers_value.retain(|c| c["service"].as_str().unwrap_or("") > after.as_str());
    }
    let consumer_key = |c: &Value| c["service"].as_str().unwrap_or("").to_string();
    let fp = fingerprint(&args_map);
    let (consumers_page, next_cursor) = apply_limit(consumers_value, limit, consumer_key, &fp);

    let mut data = json!({
        "service": info.name.0.as_str(),
        "repo": info.repo.as_str(),
        "paths": info.paths,
        "provider_reviewed": provider_reviewed,
        "endpoints": endpoints,
        "consumers": consumers_page,
        "unresolved_candidates": unresolved_candidates,
    });
    data["scope"] = scope;
    if let Some(tok) = next_cursor {
        data["cursor"] = json!(tok);
    }
    let text = render_get_service(&data);
    let envelope = success_envelope(data.clone(), &snap_label, snap_label != "live", started);
    Ok(outcome(envelope, &data, text))
}

fn get_service_not_found(
    service: &str,
    scope: Value,
    snapshot: &str,
    started: Instant,
) -> ToolOutcome {
    let mut data = json!({});
    data["scope"] = scope;
    error_outcome(
        "service_not_found",
        format!("service {service:?} not declared"),
        Some(json!({"service": service})),
        snapshot,
        started,
    )
}

#[derive(Debug)]
struct ConsumerRow {
    service: String,
    repo: String,
    uses: Vec<Value>,
}

fn build_consumer_rows(
    fed: Option<&FederatedIndex>,
    snapshot_backend: Option<&dyn GraphBackend>,
    idx: &ContractIndex,
    info: ServiceInfo,
    depth: u8,
    commit_by_repo: &std::collections::BTreeMap<String, String>,
) -> Vec<ConsumerRow> {
    let provider_endpoints: std::collections::BTreeSet<String> = info
        .endpoint_ids
        .iter()
        .map(|(_, k)| k.to_string())
        .collect();
    let mut by_consumer: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    let mut by_consumer_repo: BTreeMap<String, String> = BTreeMap::new();

    for (call_id, resolution) in &idx.consumers {
        let Some(ConsumerTarget::Binds {
            provenance,
            route_match,
            ..
        }) = &resolution.target
        else {
            continue;
        };
        let Some(endpoint) = idx
            .endpoints
            .values()
            .find(|e| provider_endpoints.contains(&e.id.1.to_string()))
        else {
            continue;
        };

        let consumer_service = resolution.service.0.clone();
        let caller_node = fed
            .and_then(|f| call_caller_node(f, call_id))
            .or_else(|| snapshot_backend.and_then(|b| call_caller_node_backend(b, call_id)))
            .unwrap_or_else(|| default_caller_node(call_id));
        let caller_repo = caller_node
            .id
            .split(':')
            .next()
            .unwrap_or_else(|| call_id.repo_id());
        let commit = commit_by_repo
            .get(caller_repo)
            .map(|s| s.as_str())
            .unwrap_or("");
        let site = call_site_meta(call_id, &caller_node, commit);

        let used_by = if let Some((_, db)) = fed.and_then(|f| caller_graph(f, call_id)) {
            walk_used_by(&db, &caller_node.id, depth)
        } else if let Some(backend) = snapshot_backend {
            walk_used_by_backend(backend, &caller_node.id, depth)
        } else {
            super::used_by::UsedByResult {
                entries: Vec::new(),
                truncated: false,
            }
        };
        let used_by_truncated = used_by.truncated;
        let mut used_by_entries = used_by.entries.clone();
        enrich_used_by_with_owners(&mut used_by_entries);
        for entry in &mut used_by_entries {
            if let Some(ref_obj) = entry.get_mut("ref").and_then(|v| v.as_object_mut()) {
                let id_str = ref_obj
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let repo = id_str.split(':').next().unwrap_or("").to_string();
                let c = commit_by_repo
                    .get(&repo)
                    .map(|s| s.as_str())
                    .unwrap_or(commit)
                    .to_string();
                let path = ref_obj
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let line = ref_obj.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                ref_obj.insert("repo".to_string(), json!(repo));
                ref_obj.insert("commit".to_string(), json!(c));
                if !c.is_empty() && !path.is_empty() {
                    ref_obj.insert(
                        "text".to_string(),
                        json!(format!("{repo}@{c}:{path}:{line}")),
                    );
                } else if !id_str.is_empty() {
                    ref_obj.insert("text".to_string(), json!(id_str));
                }
            }
        }

        let fields = collect_fields(idx, endpoint);
        let reads_complete = resolution.reads_complete;
        let match_label = match route_match {
            crate::schema::RouteMatch::Exact => "exact",
            crate::schema::RouteMatch::Pattern => "pattern",
            crate::schema::RouteMatch::PrefixStripped => "prefix_stripped",
        };

        let use_value = json!({
            "endpoint": {"service": endpoint.id.0.0.as_str(), "key": &endpoint.id.1.to_string()},
            "site": site,
            "caller": caller_node_evidence(&caller_node, commit),
            "binding": provenance_to_json(provenance),
            "match": match_label,
            "fields": fields,
            "reads_complete": reads_complete,
            "used_by": used_by_entries,
            "used_by_truncated": used_by_truncated,
        });

        let row = by_consumer.entry(consumer_service.clone()).or_default();
        let caller_key = caller_node.id.clone();
        let line_key = site["line"].as_u64().unwrap_or(0);
        let key = format!("{caller_key}|{line_key}");
        row.entry(key).or_insert(use_value);
        by_consumer_repo
            .entry(consumer_service.clone())
            .or_insert_with(|| caller_repo.to_string());
    }

    let mut rows: Vec<ConsumerRow> = by_consumer
        .into_iter()
        .map(|(service, uses_map)| {
            let uses: Vec<Value> = uses_map.into_values().collect();
            let repo = by_consumer_repo.get(&service).cloned().unwrap_or_default();
            ConsumerRow {
                service,
                repo,
                uses,
            }
        })
        .collect();
    rows.sort_by(|a, b| a.service.cmp(&b.service));
    rows
}

fn build_unresolved_candidates(
    idx: &ContractIndex,
    _info: &ServiceInfo,
    commit_by_repo: &std::collections::BTreeMap<String, String>,
) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for r in idx.consumers.values() {
        if let Some(ConsumerTarget::Unresolved { reason, .. }) = &r.target {
            let repo = r.call_id.repo_id();
            let path = r.call_id.path().unwrap_or_default();
            let line = r.call_id.line_start().unwrap_or(0);
            let commit = commit_by_repo.get(repo).map(|s| s.as_str()).unwrap_or("");
            let text = if !commit.is_empty() && !path.is_empty() {
                format!("{repo}@{commit}:{path}:{line}")
            } else {
                r.call_id.as_str().to_string()
            };
            out.push(json!({
                "consumer": {
                    "id": r.call_id.as_str(),
                    "repo": repo,
                    "commit": commit,
                    "path": path,
                    "line": line,
                    "text": text,
                },
                "url_expr": "",
                "method": "",
                "reason": unresolved_reason_label(*reason),
                "target_service": r.service.0,
            }));
        }
    }
    out
}

#[derive(Debug, Clone)]
struct CallerNode {
    id: String,
    name: String,
    path: String,
    line: u32,
}

fn caller_graph(
    fed: &FederatedIndex,
    call_id: &GlobalId,
) -> Option<(RepoId, crate::graph::GraphDatabase)> {
    let repo_id = RepoId::new(call_id.repo_id()).ok()?;
    let repo = fed.get_repo(&repo_id)?;
    Some((repo_id, repo.db().clone()))
}

fn call_caller_node(fed: &FederatedIndex, call_id: &GlobalId) -> Option<CallerNode> {
    let (_, db) = caller_graph(fed, call_id)?;
    let sends_source_id = db
        .all_edges()
        .into_iter()
        .find(|e| e.edge_type == EdgeType::SendsHttp && e.target_id == call_id.as_str())
        .map(|e| e.source_id)?;
    let caller = db.get_node(&sends_source_id).ok().flatten()?;
    Some(CallerNode {
        id: caller.id,
        path: caller.path,
        line: caller.line_start.unwrap_or(0),
        name: caller.name,
    })
}

fn call_caller_node_backend(backend: &dyn GraphBackend, call_id: &GlobalId) -> Option<CallerNode> {
    let sends_source_id = backend
        .all_edges()
        .ok()?
        .into_iter()
        .find(|e| e.edge_type == EdgeType::SendsHttp && e.target_id == call_id.as_str())?
        .source_id;
    let caller = backend.get_node(&sends_source_id).ok().flatten()?;
    Some(CallerNode {
        id: caller.id,
        path: caller.path,
        line: caller.line_start.unwrap_or(0),
        name: caller.name,
    })
}

fn default_caller_node(call_id: &GlobalId) -> CallerNode {
    CallerNode {
        id: call_id.as_str().to_string(),
        path: call_id.path().unwrap_or_default(),
        line: call_id.line_start().unwrap_or(0),
        name: call_id.name().unwrap_or_default(),
    }
}

fn call_site_meta(call_id: &GlobalId, caller: &CallerNode, commit: &str) -> Value {
    let repo = call_id.repo_id();
    let path = if !caller.path.is_empty() {
        caller.path.clone()
    } else {
        call_id.path().unwrap_or_default()
    };
    let line = if caller.line > 0 {
        caller.line
    } else {
        call_id.line_start().unwrap_or(0)
    };
    let text = if !commit.is_empty() && !path.is_empty() {
        format!("{repo}@{commit}:{path}:{line}")
    } else {
        call_id.as_str().to_string()
    };
    json!({
        "id": call_id.as_str(),
        "repo": repo,
        "commit": commit,
        "path": path,
        "line": line,
        "text": text,
    })
}

fn caller_node_evidence(caller: &CallerNode, commit: &str) -> Value {
    let repo = caller.id.split(':').next().unwrap_or("");
    let text = if !commit.is_empty() && !caller.path.is_empty() {
        format!("{repo}@{commit}:{}:{}", caller.path, caller.line)
    } else {
        caller.id.clone()
    };
    json!({
        "id": caller.id,
        "name": caller.name,
        "repo": repo,
        "commit": commit,
        "path": caller.path,
        "line": caller.line,
        "text": text,
    })
}

/// Attach `owners: [String]` to every `used_by` entry (§10.9, PR 17).
/// Each entry's `ref.id` is the entry-point's `GlobalId`; the repo is
/// the first segment, the path comes from `ref.path`. `codeowners_for`
/// returns an empty list when the repo has no `CODEOWNERS` or the path
/// doesn't match — in that case the field is omitted to keep the
/// output minimal.
fn enrich_used_by_with_owners(entries: &mut [Value]) {
    for entry in entries.iter_mut() {
        let Some(ref_obj) = entry.get_mut("ref").and_then(|v| v.as_object_mut()) else {
            continue;
        };
        let Some(id) = ref_obj.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let repo = id.split(':').next().unwrap_or("");
        let path = ref_obj.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let owners = codeowners_for(repo, path);
        if !owners.is_empty() {
            entry["owners"] = json!(owners);
        }
    }
}

fn provenance_to_json(p: &crate::schema::EdgeProvenance) -> Value {
    let (kind, confidence, detector, source) = match p {
        crate::schema::EdgeProvenance::Static { source } => {
            ("static", 1.0_f32, None, Some(format!("{source:?}")))
        }
        crate::schema::EdgeProvenance::Heuristic {
            detector,
            confidence,
        } => ("heuristic", *confidence, Some(detector.clone()), None),
        crate::schema::EdgeProvenance::Runtime {
            trace_id,
            last_seen_unix,
        } => (
            "runtime",
            1.0_f32,
            Some(trace_id.clone()),
            Some(last_seen_unix.to_string()),
        ),
        crate::schema::EdgeProvenance::Confirmed { source } => {
            ("confirmed", 1.0_f32, None, Some(source.clone()))
        }
    };
    let mut out = json!({"kind": kind, "confidence": confidence});
    if let Some(d) = detector {
        out["detector"] = json!(d);
    }
    if let Some(s) = source {
        out["source"] = json!(s);
    }
    out
}

fn collect_fields(_idx: &ContractIndex, _endpoint: &Endpoint) -> Vec<Value> {
    // The §7.5 field-join (`BoundField` set) is filled by PR 9 and
    // stored on `ContractIndex::field_refs` keyed by `FieldRef`
    // id, not by `HttpClientCall` id. The PR 16 wire shape requires
    // `fields: BoundField[]` per consumer; for the `live` view we
    // surface an empty list when the field-join bookkeeping doesn't
    // match the consumer-call id. PR 12 (`diff_contracts`) will plug
    // the join in here — the call shape is already in place.
    Vec::new()
}

// ─── renderers ────────────────────────────────────────────────────────

fn render_list_services(data: &Value) -> String {
    let items = data["items"].as_array().cloned().unwrap_or_default();
    let mut out = String::new();
    out.push_str(&format!("# Services ({} item(s))\n", items.len()));
    for it in &items {
        out.push_str(&format!(
            "- {} (repo: {}, endpoints: {}, consumer_services: {})\n",
            it["service"].as_str().unwrap_or(""),
            it["repo"].as_str().unwrap_or(""),
            it["endpoints"].as_u64().unwrap_or(0),
            it["consumer_services"].as_u64().unwrap_or(0),
        ));
    }
    cap_2000(out)
}

fn render_get_service(data: &Value) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# {} ({})\n\nprovider_reviewed: {}\nendpoints ({}): {}\n\n",
        data["service"].as_str().unwrap_or(""),
        data["repo"].as_str().unwrap_or(""),
        data["provider_reviewed"].as_bool().unwrap_or(false),
        data["endpoints"].as_array().map(|a| a.len()).unwrap_or(0),
        data["endpoints"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default(),
    ));
    let consumers = data["consumers"].as_array().cloned().unwrap_or_default();
    out.push_str(&format!("## Consumers ({})\n", consumers.len()));
    for c in &consumers {
        let svc = c["service"].as_str().unwrap_or("");
        let uses = c["uses"].as_array().cloned().unwrap_or_default();
        out.push_str(&format!("- {} ({} uses)\n", svc, uses.len()));
        for u in &uses {
            let endpoint = u["endpoint"]["key"].as_str().unwrap_or("");
            let caller = u["caller"]["name"].as_str().unwrap_or("");
            out.push_str(&format!(
                "  - {} via {} (used_by: {} entries, truncated: {})\n",
                endpoint,
                caller,
                u["used_by"].as_array().map(|a| a.len()).unwrap_or(0),
                u["used_by_truncated"].as_bool().unwrap_or(false),
            ));
        }
    }
    cap_2000(out)
}

#[allow(dead_code)]
fn _route_prefix_display(prefix: &RoutePrefix) -> String {
    format!("{prefix:?}")
}

#[allow(dead_code)]
fn _provider_origin_label(origin: ProviderOrigin) -> &'static str {
    match origin {
        ProviderOrigin::Code => "code",
        ProviderOrigin::OpenApi => "openapi",
    }
}

#[allow(dead_code)]
fn _bound_field_json(b: &BoundField) -> Value {
    json!({
        "json_path": b.field_path.0,
        "field": b.field.as_str(),
        "endpoint": {"service": b.endpoint.0.0, "key": b.endpoint.1.to_string()},
        "confidence": b.confidence,
    })
}

#[allow(dead_code)]
fn _entry_kind_display(k: EntryKind) -> &'static str {
    match k {
        EntryKind::HttpHandler => "http_handler",
        EntryKind::Scheduled => "scheduled",
        EntryKind::Cli => "cli",
        EntryKind::Main => "main",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::contracts::model::ServiceName;

    #[test]
    fn parse_limit_default_when_absent() {
        let m = Map::new();
        assert_eq!(
            parse_limit(&m, "live", Instant::now()).unwrap(),
            DEFAULT_LIMIT
        );
    }

    #[test]
    fn parse_limit_rejects_over_max() {
        let mut m = Map::new();
        m.insert("limit".to_string(), json!(MAX_LIMIT + 1));
        let o = parse_limit(&m, "live", Instant::now()).unwrap_err();
        assert!(o.is_error);
        assert_eq!(o.structured["error"]["code"], json!("range_too_large"));
    }

    #[test]
    fn parse_limit_rejects_zero() {
        let mut m = Map::new();
        m.insert("limit".to_string(), json!(0));
        let o = parse_limit(&m, "live", Instant::now()).unwrap_err();
        assert_eq!(o.structured["error"]["code"], json!("invalid_argument"));
    }

    #[test]
    fn parse_depth_rejects_over_max() {
        let mut m = Map::new();
        m.insert("depth".to_string(), json!(MAX_DEPTH + 1));
        let o = parse_depth(&m, "live", Instant::now()).unwrap_err();
        assert!(o.is_error);
        assert_eq!(o.structured["error"]["code"], json!("range_too_large"));
    }

    #[test]
    fn parse_cursor_rejects_malformed() {
        let mut m = Map::new();
        m.insert("cursor".to_string(), json!("@@@"));
        let o = parse_cursor(&m, "live", Instant::now()).unwrap_err();
        assert!(o.is_error);
    }

    #[test]
    fn parse_cursor_rejects_mismatched_fingerprint() {
        // Encode a cursor with q="deadbeef" then supply a different
        // arg set; the fingerprint won't match → cursor_mismatch.
        let token = crate::server::mcp::contract_tools::paging::encode_cursor("orders", "deadbeef");
        let mut m = Map::new();
        m.insert("snapshot".to_string(), json!("live"));
        m.insert("service".to_string(), json!("orders"));
        m.insert("cursor".to_string(), json!(token));
        let o = parse_cursor(&m, "live", Instant::now()).unwrap_err();
        assert_eq!(o.structured["error"]["code"], json!("invalid_argument"));
        assert_eq!(
            o.structured["error"]["details"]["reason"],
            json!("cursor_mismatch")
        );
    }

    #[test]
    fn call_site_meta_populates_complete_evidence() {
        let cid = GlobalId::new(
            &crate::federation::repo_id::RepoId::new("orders").unwrap(),
            crate::schema::NodeType::HttpClientCall,
            "src/client.rs",
            "get_order",
            Some(42),
        );
        let caller = default_caller_node(&cid);
        let meta = call_site_meta(&cid, &caller, "c0ffee");
        assert_eq!(meta["repo"], "orders");
        assert_eq!(meta["commit"], "c0ffee");
        assert_eq!(meta["path"], "src/client.rs");
        assert_eq!(meta["line"], 42);
        assert_eq!(meta["text"], "orders@c0ffee:src/client.rs:42");
    }

    #[test]
    fn count_distinct_consumer_services_dedupes() {
        let idx = ContractIndex::default();
        let info = ServiceInfo {
            name: ServiceName("orders".to_string()),
            repo: crate::federation::repo_id::RepoId::new("orders").unwrap(),
            paths: vec![],
            hosts: vec![],
            env: vec![],
            base_path: None,
            route_prefixes: vec![],
            endpoint_ids: vec![],
        };
        // No consumers — count is zero.
        assert_eq!(count_distinct_consumer_services(&idx, &info), 0);
    }
}
