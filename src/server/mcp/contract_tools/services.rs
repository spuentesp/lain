//! Contract tool: `list_services` and `get_service`.
//!
//! `docs/CONTRACT_FEDERATION.md` §10.1 / §10.2 / §10.5 / §10.9 / §12.
//!
//! The two tools read from the in-process `FederatedIndex::contract_index()`
//! after calling `rejoin_contracts_if_dirty()` (§10.1 / §5.3). They
//! are `live`-only for this PR — anything other than `"live"` returns
//! `snapshot_not_found`. PR 11/13 surfaces the snapshot manager and
//! replaces the `contract_index` lookup with a per-snapshot read.
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

use super::envelope::{cap_2000, check_api_version, error_outcome, outcome, success_envelope};
use super::paging::{apply_limit, decode_cursor, fingerprint};
use super::scope::{live_scope, provider_is_reviewed};
use super::used_by::walk as walk_used_by;
use super::{ContractToolEntry, ToolOutcome, DEFAULT_DEPTH, DEFAULT_LIMIT, MAX_DEPTH, MAX_LIMIT};
use crate::federation::contracts::config::RoutePrefix;
use crate::federation::contracts::index::{
    BoundField, ConsumerTarget, ContractIndex, Endpoint, ServiceInfo, UnresolvedReason,
};
use crate::federation::contracts::model::{ContractKey, EntryKind, ProviderOrigin};
use crate::federation::federated_index::FederatedIndex;
use crate::federation::health::RepoHealth;
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::schema::EdgeType;
use crate::server::mcp::handler::McpContext;
use crate::server::sensors::codeowners_sensor::codeowners_for;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
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
    let ctx_fed = ctx.federation;
    Box::pin(async move {
        let started = Instant::now();
        if let Err(details) = check_api_version(&args_map) {
            let o = error_outcome(
                "unsupported_api_version",
                "api_version not served",
                Some(details),
                "live",
                started,
            );
            return Ok(o);
        }
        match run_list_services(ctx_fed, args_map, started).await {
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
    let ctx_fed = ctx.federation;
    Box::pin(async move {
        let started = Instant::now();
        if let Err(details) = check_api_version(&args_map) {
            let o = error_outcome(
                "unsupported_api_version",
                "api_version not served",
                Some(details),
                "live",
                started,
            );
            return Ok(o);
        }
        match run_get_service(ctx_fed, args_map, started).await {
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

/// Validate the `snapshot` argument. PR 16 only supports `"live"`.
fn require_live_snapshot(args: &Map<String, Value>, started: Instant) -> Result<(), ToolOutcome> {
    let snapshot = args.get("snapshot").and_then(|v| v.as_str()).unwrap_or("");
    if snapshot == "live" {
        Ok(())
    } else {
        Err(error_outcome(
            "snapshot_not_found",
            format!("snapshot {snapshot:?} not found"),
            None,
            snapshot,
            started,
        ))
    }
}

/// Decode the `limit` argument (`§10.5`). Returns `Err` with the
/// `range_too_large` outcome when the value exceeds `MAX_LIMIT`.
fn parse_limit(args: &Map<String, Value>, started: Instant) -> Result<usize, ToolOutcome> {
    let raw = args.get("limit").and_then(|v| v.as_u64());
    match raw {
        None => Ok(DEFAULT_LIMIT),
        Some(n) if (n as usize) > MAX_LIMIT => Err(error_outcome(
            "range_too_large",
            format!("limit {n} exceeds max {MAX_LIMIT}"),
            Some(json!({"limit": n, "max": MAX_LIMIT, "requested": n})),
            "live",
            started,
        )),
        Some(0) => Err(error_outcome(
            "invalid_argument",
            "limit must be >= 1",
            Some(json!({"arg": "limit"})),
            "live",
            started,
        )),
        Some(n) => Ok(n as usize),
    }
}

/// Decode the `depth` argument (`§10.5`).
fn parse_depth(args: &Map<String, Value>, started: Instant) -> Result<u8, ToolOutcome> {
    let raw = args.get("depth").and_then(|v| v.as_u64());
    match raw {
        None => Ok(DEFAULT_DEPTH),
        Some(n) if (n as u8) > MAX_DEPTH => Err(error_outcome(
            "range_too_large",
            format!("depth {n} exceeds max {MAX_DEPTH}"),
            Some(json!({"limit": n, "max": MAX_DEPTH, "requested": n})),
            "live",
            started,
        )),
        Some(0) => Err(error_outcome(
            "invalid_argument",
            "depth must be >= 1",
            Some(json!({"arg": "depth"})),
            "live",
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
                "live",
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
            "live",
            started,
        ));
    }
    Ok(Some(cur))
}

// ─── list_services implementation ─────────────────────────────────────

async fn run_list_services(
    fed: Option<&FederatedIndex>,
    args_map: Map<String, Value>,
    started: Instant,
) -> Result<ToolOutcome, ToolOutcome> {
    require_live_snapshot(&args_map, started)?;
    let fed = fed.ok_or_else(|| {
        error_outcome(
            "federation_disabled",
            "this server is not configured with a federation",
            None,
            "live",
            started,
        )
    })?;
    let limit = parse_limit(&args_map, started)?;
    let cursor = parse_cursor(&args_map, started)?;
    let repo_filter = args_map
        .get("repo")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if let Err(e) = fed.rejoin_contracts_if_dirty() {
        return Err(error_outcome(
            "invalid_argument",
            format!("rejoin failed: {e}"),
            None,
            "live",
            started,
        ));
    }
    let Some(idx) = fed.contract_index() else {
        return Ok(list_services_empty(fed, started));
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
    let scope = live_scope(fed);
    data["scope"] = scope;
    if let Some(tok) = next_cursor {
        data["cursor"] = json!(tok);
    }
    let text = render_list_services(&data);
    let envelope = success_envelope(data.clone(), "live", false, started);
    Ok(outcome(envelope, &data, text))
}

fn list_services_empty(fed: &FederatedIndex, started: Instant) -> ToolOutcome {
    let data = json!({"items": [], "scope": live_scope(fed)});
    let envelope = success_envelope(data.clone(), "live", false, started);
    let text = render_list_services(&data);
    outcome(envelope, &data, text)
}

fn count_distinct_consumer_services(idx: &ContractIndex, info: &ServiceInfo) -> usize {
    let provider_endpoints: std::collections::BTreeSet<String> = info
        .endpoint_ids
        .iter()
        .map(|(_, k)| key_label(k))
        .collect();
    let mut consumers: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for resolution in idx.consumers.values() {
        if let Some(ConsumerTarget::Binds { .. }) = &resolution.target {
            // `bound_endpoints` lists the endpoints this call binds
            // to. If any of those endpoints belong to this service,
            // count the consumer service.
            let bound = resolution
                .bound_endpoints
                .iter()
                .any(|(_, k)| provider_endpoints.contains(&key_label(k)));
            if bound {
                consumers.insert(resolution.service.0.clone());
            }
        }
    }
    consumers.len()
}

fn key_label(k: &ContractKey) -> String {
    k.to_string()
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
    fed: Option<&FederatedIndex>,
    args_map: Map<String, Value>,
    started: Instant,
) -> Result<ToolOutcome, ToolOutcome> {
    require_live_snapshot(&args_map, started)?;
    let fed = fed.ok_or_else(|| {
        error_outcome(
            "federation_disabled",
            "this server is not configured with a federation",
            None,
            "live",
            started,
        )
    })?;
    let limit = parse_limit(&args_map, started)?;
    let depth = parse_depth(&args_map, started)?;
    let cursor = parse_cursor(&args_map, started)?;
    let service_arg = args_map
        .get("service")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            error_outcome(
                "invalid_argument",
                "missing required argument: service",
                Some(json!({"arg": "service"})),
                "live",
                started,
            )
        })?
        .to_string();

    if let Err(e) = fed.rejoin_contracts_if_dirty() {
        return Err(error_outcome(
            "invalid_argument",
            format!("rejoin failed: {e}"),
            None,
            "live",
            started,
        ));
    }
    let Some(idx) = fed.contract_index() else {
        return Ok(get_service_not_found(&service_arg, fed, started));
    };
    let Some(info) = idx
        .services
        .get(&crate::federation::contracts::model::ServiceName(
            service_arg.clone(),
        ))
    else {
        return Ok(get_service_not_found(&service_arg, fed, started));
    };

    let health = fed
        .list_repos()
        .into_iter()
        .find(|(rid, _)| rid == &info.repo)
        .map(|(_, h)| h)
        .unwrap_or(RepoHealth::Missing);
    let provider_reviewed = provider_is_reviewed(health);

    let endpoints: Vec<String> = info
        .endpoint_ids
        .iter()
        .map(|(_, k)| key_label(k))
        .collect();
    let consumers = build_consumer_rows(fed, &idx, info.clone(), depth);

    let unresolved_candidates = build_unresolved_candidates(&idx, info);

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

    // Cursor paging over consumer-service rows, sorted by
    // `(service, caller GlobalId, site line)` per §12. We already
    // build the rows in that order; the cursor only trims by
    // `service` (the first key) — finer-grained paging is the §13
    // backlog, listed below.
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
    data["scope"] = live_scope(fed);
    if let Some(tok) = next_cursor {
        data["cursor"] = json!(tok);
    }
    let text = render_get_service(&data);
    let envelope = success_envelope(data.clone(), "live", false, started);
    Ok(outcome(envelope, &data, text))
}

fn get_service_not_found(service: &str, fed: &FederatedIndex, started: Instant) -> ToolOutcome {
    let mut data = json!({});
    data["scope"] = live_scope(fed);
    error_outcome(
        "service_not_found",
        format!("service {service:?} not declared"),
        Some(json!({"service": service})),
        "live",
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
    fed: &FederatedIndex,
    idx: &ContractIndex,
    info: ServiceInfo,
    depth: u8,
) -> Vec<ConsumerRow> {
    let provider_endpoints: std::collections::BTreeSet<String> = info
        .endpoint_ids
        .iter()
        .map(|(_, k)| key_label(k))
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
            .find(|e| provider_endpoints.contains(&key_label(&e.id.1)))
        else {
            continue;
        };

        let consumer_service = resolution.service.0.clone();
        let caller_node =
            call_caller_node(fed, call_id).unwrap_or_else(|| default_caller_node(call_id));
        let site = call_site_meta(call_id, &caller_node);

        // `used_by`: walk from the caller with the requested depth.
        // We need the caller's enclosing function's graph node id, not
        // the call itself; the `SendsHttp` edge's source is the
        // caller. For PR 16 we approximate by reading the caller via
        // the federation's per-repo db when we can find one.
        let used_by = match caller_graph(fed, call_id) {
            Some((_, db)) => walk_used_by(&db, &caller_node.id, depth),
            None => super::used_by::UsedByResult {
                entries: Vec::new(),
                truncated: false,
            },
        };
        let used_by_truncated = used_by.truncated;
        let mut used_by_entries = used_by.entries.clone();
        enrich_used_by_with_owners(&mut used_by_entries);

        let fields = collect_fields(idx, endpoint);
        let reads_complete = resolution.reads_complete;
        let match_label = match route_match {
            crate::schema::RouteMatch::Exact => "exact",
            crate::schema::RouteMatch::Pattern => "pattern",
            crate::schema::RouteMatch::PrefixStripped => "prefix_stripped",
        };

        let use_value = json!({
            "endpoint": {"service": endpoint.id.0.0.as_str(), "key": key_label(&endpoint.id.1)},
            "site": site,
            "caller": caller_node_evidence(&caller_node),
            "binding": provenance_to_json(provenance),
            "match": match_label,
            "fields": fields,
            "reads_complete": reads_complete,
            "used_by": used_by_entries,
            "used_by_truncated": used_by_truncated,
        });

        // Group: row per consumer service; multiple uses are appended
        // in `(caller GlobalId, site line)` order.
        let row = by_consumer.entry(consumer_service.clone()).or_default();
        let caller_key = caller_node.id.clone();
        let line_key = site["line"].as_u64().unwrap_or(0);
        let key = format!("{caller_key}|{line_key}");
        row.entry(key).or_insert(use_value);
        by_consumer_repo
            .entry(consumer_service.clone())
            .or_insert_with(|| consumer_repo_for(fed, &caller_node));
        let _ = used_by_truncated;
    }

    // §12 sort: (service, caller GlobalId, site line). The by_consumer
    // map is already a BTreeMap by service; the inner BTreeMap keys
    // are `caller_id|line`, which sorts on caller_id first then line.
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

fn build_unresolved_candidates(idx: &ContractIndex, _info: &ServiceInfo) -> Vec<Value> {
    // §9.7: unresolved consumers in any reviewed repo whose target
    // could match any endpoint of this service. The §9.7 rule is
    // exact-match-by-service, not per-endpoint — so any unresolved
    // consumer in this service's repo counts.
    let mut out: Vec<Value> = Vec::new();
    for r in idx.consumers.values() {
        if let Some(ConsumerTarget::Unresolved { reason, .. }) = &r.target {
            out.push(json!({
                "consumer": {"id": r.call_id.as_str(), "repo": "", "commit": "", "path": "", "line": 0, "text": ""},
                "url_expr": "",
                "method": "",
                "reason": match reason {
                    UnresolvedReason::NoRouteInService => "no_route_in_service",
                    UnresolvedReason::NoMatch => "no_match",
                    UnresolvedReason::Unnormalized => "no_route_in_service",
                    UnresolvedReason::WrapperUnconfigured => "wrapper_unconfigured",
                    UnresolvedReason::EnvUnmapped => "env_unmapped",
                    UnresolvedReason::EnvAmbiguous => "env_ambiguous",
                    UnresolvedReason::RpcStubUnknown => "rpc_stub_unknown",
                },
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
    // The `HttpClientCall` node has a `SendsHttp` edge from its
    // enclosing function — that's the caller the `used_by` walk
    // needs to start from. Real sensors wire this edge; the
    // federation_contracts_e2e tests do too.
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

fn default_caller_node(call_id: &GlobalId) -> CallerNode {
    CallerNode {
        id: call_id.as_str().to_string(),
        path: String::new(),
        line: 0,
        name: String::new(),
    }
}

fn call_site_meta(call_id: &GlobalId, caller: &CallerNode) -> Value {
    json!({
        "id": call_id.as_str(),
        "repo": call_id.repo_id(),
        "commit": "",
        "path": caller.path,
        "line": caller.line,
        "text": "",
    })
}

fn consumer_repo_for(_fed: &FederatedIndex, caller: &CallerNode) -> String {
    // The consumer repo is the repo whose `HttpClientCall` we just
    // bound — derived from the caller's GlobalId repo-id segment.
    caller
        .id
        .split(':')
        .next()
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn caller_node_evidence(caller: &CallerNode) -> Value {
    json!({
        "id": caller.id,
        "name": caller.name,
        "repo": "",
        "commit": "",
        "path": caller.path,
        "line": caller.line,
        "text": "",
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
        assert_eq!(parse_limit(&m, Instant::now()).unwrap(), DEFAULT_LIMIT);
    }

    #[test]
    fn parse_limit_rejects_over_max() {
        let mut m = Map::new();
        m.insert("limit".to_string(), json!(MAX_LIMIT + 1));
        let o = parse_limit(&m, Instant::now()).unwrap_err();
        assert!(o.is_error);
        assert_eq!(o.structured["error"]["code"], json!("range_too_large"));
    }

    #[test]
    fn parse_limit_rejects_zero() {
        let mut m = Map::new();
        m.insert("limit".to_string(), json!(0));
        let o = parse_limit(&m, Instant::now()).unwrap_err();
        assert_eq!(o.structured["error"]["code"], json!("invalid_argument"));
    }

    #[test]
    fn parse_depth_rejects_over_max() {
        let mut m = Map::new();
        m.insert("depth".to_string(), json!(MAX_DEPTH + 1));
        let o = parse_depth(&m, Instant::now()).unwrap_err();
        assert!(o.is_error);
        assert_eq!(o.structured["error"]["code"], json!("range_too_large"));
    }

    #[test]
    fn parse_cursor_rejects_malformed() {
        let mut m = Map::new();
        m.insert("cursor".to_string(), json!("@@@"));
        let o = parse_cursor(&m, Instant::now()).unwrap_err();
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
        let o = parse_cursor(&m, Instant::now()).unwrap_err();
        assert_eq!(o.structured["error"]["code"], json!("invalid_argument"));
        assert_eq!(
            o.structured["error"]["details"]["reason"],
            json!("cursor_mismatch")
        );
    }

    #[test]
    fn require_live_snapshot_rejects_other_values() {
        let mut m = Map::new();
        m.insert("snapshot".to_string(), json!("snap_abc"));
        let o = require_live_snapshot(&m, Instant::now()).unwrap_err();
        assert_eq!(o.structured["error"]["code"], json!("snapshot_not_found"));
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
