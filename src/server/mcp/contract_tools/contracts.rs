//! Contract tools: `list_contracts`, `get_contract`, `list_unresolved`,
//! `check_binding` (`docs/CONTRACT_FEDERATION.md` §12 + §13).
//!
//! Read-only views over the federation's `ContractIndex` (live) or
//! a named snapshot's `ContractIndex` (`from_snapshot` projection).
//! All four tools share the §10.2 envelope and the §10.5 paging +
//! limits logic; `check_binding` is the only one that takes a
//! proposed consumer→endpoint pair and emits a `bindings_entry`
//! YAML for `repos.yaml`.
//!
//! ## Sort orders (§12)
//!
//! - `list_contracts.items`: by `(service, key)` (the joiner's
//!   endpoint-id order).
//! - `list_unresolved.items`: by `consumer.id`.
//!
//! ## Limits (§10.5)
//!
//! - `list` `limit`: default 100, max 1000; `range_too_large` beyond.
//! - `cursor`: opaque base64url; `cursor_mismatch` on reuse with
//!   different arguments.
//!
//! ## Sources
//!
//! On `live`, read `fed.contract_index()`. On `snap_<id>`, the
//! `from_snapshot` projection has already populated the
//! `SnapshotFederation.contract_index`; we route through the
//! `McpContext::snapshots` handle to load it.
//!
//! ## Idempotence / determinism
//!
//! Every collection is sorted by the documented key; the in-memory
//! `ContractIndex` already uses `BTreeMap`, so output is
//! byte-identical across calls for the same view (modulo `meta`).

use super::envelope::{check_api_version, error_outcome, outcome, success_envelope};
use super::paging::{apply_limit, decode_cursor, fingerprint};
use super::scope::live_scope;
use super::{ContractToolEntry, ContractToolFuture, ToolOutcome};
use crate::federation::contracts::index::{
    ConsumerResolution, ConsumerTarget, ContractIndex, Endpoint, EndpointId, UnresolvedReason,
};
use crate::federation::contracts::model::MethodSpec;
use crate::federation::contracts::model::{ContractKey, Direction, ServiceName};
use crate::federation::contracts::route_match::match_route;
use crate::federation::repo_id::GlobalId;
use crate::schema::{EdgeProvenance, RouteMatch};
use crate::server::mcp::handler::McpContext;
use serde_json::{json, Map, Value};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

// ─── dispatch ─────────────────────────────────────────────────────────

inventory::submit!(ContractToolEntry {
    name: "list_contracts",
    handler: list_contracts_handle,
});
inventory::submit!(ContractToolEntry {
    name: "get_contract",
    handler: get_contract_handle,
});
inventory::submit!(ContractToolEntry {
    name: "list_unresolved",
    handler: list_unresolved_handle,
});
inventory::submit!(ContractToolEntry {
    name: "check_binding",
    handler: check_binding_handle,
});

/// `list_contracts` async handler.
pub fn list_contracts_handle<'a>(ctx: &'a McpContext<'a>, args: Value) -> ContractToolFuture<'a> {
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
        match run_list_contracts(ctx, args_map, started).await {
            Ok(o) => Ok(o),
            Err(o) => Ok(o),
        }
    })
}

/// `get_contract` async handler.
pub fn get_contract_handle<'a>(ctx: &'a McpContext<'a>, args: Value) -> ContractToolFuture<'a> {
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
        match run_get_contract(ctx, args_map, started).await {
            Ok(o) => Ok(o),
            Err(o) => Ok(o),
        }
    })
}

/// `list_unresolved` async handler.
pub fn list_unresolved_handle<'a>(ctx: &'a McpContext<'a>, args: Value) -> ContractToolFuture<'a> {
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
        match run_list_unresolved(ctx, args_map, started).await {
            Ok(o) => Ok(o),
            Err(o) => Ok(o),
        }
    })
}

/// `check_binding` async handler.
pub fn check_binding_handle<'a>(ctx: &'a McpContext<'a>, args: Value) -> ContractToolFuture<'a> {
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
        match run_check_binding(ctx, args_map, started).await {
            Ok(o) => Ok(o),
            Err(o) => Ok(o),
        }
    })
}

// ─── list_contracts implementation ────────────────────────────────────

async fn run_list_contracts(
    ctx: &McpContext<'_>,
    args_map: Map<String, Value>,
    started: Instant,
) -> Result<ToolOutcome, ToolOutcome> {
    let snap_label = snapshot_label(&args_map);
    let view = match resolve_view(ctx, &args_map, started).await? {
        ViewHandle::Empty(_reason) => {
            let scope = live_scope_when_live(ctx, &args_map);
            return Ok(empty_outcome(scope, "list_contracts", &snap_label, started));
        }
        ViewHandle::Index { index, .. } => index,
    };
    let limit = parse_limit(&args_map, started)?;
    let cursor = parse_cursor(&args_map, started)?;
    let service_filter = args_map
        .get("service")
        .and_then(|v| v.as_str())
        .map(|s| ServiceName(s.to_string()));
    let repo_filter = args_map
        .get("repo")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let kind_filter = args_map
        .get("kind")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mut items: Vec<Value> = Vec::new();
    for (endpoint_id, endpoint) in &view.endpoints {
        if let Some(ref svc) = service_filter {
            if &endpoint_id.0 != svc {
                continue;
            }
        }
        if let Some(ref repo) = repo_filter {
            if !endpoint
                .providers
                .iter()
                .any(|p| p.node_id.repo_id() == repo.as_str())
            {
                continue;
            }
        }
        if let Some(ref k) = kind_filter {
            let key_kind = match &endpoint_id.1 {
                ContractKey::Http { .. } => "http",
                ContractKey::Topic { .. } => "topic",
            };
            if k != key_kind {
                continue;
            }
        }
        let bound = bound_consumers_count(&view, endpoint_id);
        let providers_json: Vec<Value> = endpoint
            .providers
            .iter()
            .map(|p| {
                evidence_ref(
                    &p.node_id,
                    &p.node_id.path().unwrap_or_default(),
                    p.node_id.line_start().unwrap_or(0),
                )
            })
            .collect();
        items.push(json!({
            "endpoint": {"service": endpoint_id.0 .0, "key": endpoint_id.1.to_string()},
            "providers": providers_json,
            "has_schema": endpoint.schemas.values().any(|s| !s.fields.is_empty()),
            "bound_consumers": bound,
        }));
    }
    let key = |it: &Value| {
        format!(
            "{}|{}",
            it["endpoint"]["service"].as_str().unwrap_or(""),
            it["endpoint"]["key"].as_str().unwrap_or("")
        )
    };
    if let Some(after) = cursor.as_ref().map(|c| c.after.clone()) {
        items.retain(|it| key(it) > after);
    }
    let fp = fingerprint(&args_map);
    let (page, next_cursor) = apply_limit(items, limit, key, &fp);

    let mut data = json!({"items": page});
    if let Some(tok) = next_cursor {
        data["cursor"] = json!(tok);
    }
    data["scope"] = scope_for_view(ctx, &args_map);
    let envelope = success_envelope(data.clone(), &snap_label, snap_label != "live", started);
    let text = render_list_contracts(&data);
    Ok(outcome(envelope, &data, text))
}

// ─── get_contract implementation ──────────────────────────────────────

async fn run_get_contract(
    ctx: &McpContext<'_>,
    args_map: Map<String, Value>,
    started: Instant,
) -> Result<ToolOutcome, ToolOutcome> {
    let snap_label = snapshot_label(&args_map);
    let view = match resolve_view(ctx, &args_map, started).await? {
        ViewHandle::Empty(_) => {
            return Ok(error_outcome(
                "contract_not_found",
                "no contract index available for this snapshot",
                Some(json!({"endpoint": null})),
                &snap_label,
                started,
            ));
        }
        ViewHandle::Index { index, .. } => index,
    };
    let key_str = args_map
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            error_outcome(
                "invalid_argument",
                "missing required argument: key",
                Some(json!({"arg": "key"})),
                &snap_label,
                started,
            )
        })?;
    let key = ContractKey::from_str(key_str).map_err(|e| {
        error_outcome(
            "invalid_argument",
            format!("malformed key: {e}"),
            Some(json!({"arg": "key", "reason": "malformed"})),
            &snap_label,
            started,
        )
    })?;
    let service_filter = args_map
        .get("service")
        .and_then(|v| v.as_str())
        .map(|s| ServiceName(s.to_string()));

    // §12: no `service` → every service providing key. With `service`
    // → only that service's endpoint id. The joiner guarantees
    // endpoints keyed per service so the same key served by two
    // services never collides (§9.1).
    let mut matched: Vec<(&EndpointId, &Endpoint)> = Vec::new();
    for (endpoint_id, endpoint) in &view.endpoints {
        if endpoint_id.1 != key {
            continue;
        }
        if let Some(ref svc) = service_filter {
            if &endpoint_id.0 != svc {
                continue;
            }
        }
        matched.push((endpoint_id, endpoint));
    }

    if matched.is_empty() {
        return Err(error_outcome(
            "contract_not_found",
            format!("contract {key_str} not found"),
            Some(json!({"endpoint": {"key": key_str}})),
            &snap_label,
            started,
        ));
    }

    let mut items: Vec<Value> = Vec::new();
    for (endpoint_id, endpoint) in &matched {
        let mut schemas: Vec<Value> = Vec::new();
        for dir in [Direction::Request, Direction::Response, Direction::Payload] {
            if let Some(schema) = endpoint.schemas.get(&dir) {
                let mut fields: Vec<Value> = Vec::new();
                for (path, meta) in &schema.fields {
                    fields.push(json!({
                        "json_path": path.to_string(),
                        "ty": type_desc_label(&meta.ty),
                        "required": meta.required,
                        "nullable": meta.nullable,
                        "enum_values": meta.enum_values.clone(),
                        "ref": evidence_ref(&schema.node_id, &path.to_string(), 0),
                    }));
                }
                schemas.push(json!({
                    "direction": direction_label(dir),
                    "fields": fields,
                }));
            }
        }
        let consumers = collect_consumers_for_endpoint(&view, endpoint_id);
        let providers: Vec<Value> = endpoint
            .providers
            .iter()
            .map(|p| {
                evidence_ref(
                    &p.node_id,
                    &p.node_id.path().unwrap_or_default(),
                    p.node_id.line_start().unwrap_or(0),
                )
            })
            .collect();
        items.push(json!({
            "endpoint": {"service": endpoint_id.0 .0, "key": endpoint_id.1.to_string()},
            "providers": providers,
            "schemas": schemas,
            "consumers": consumers,
        }));
    }

    let mut data = json!({"items": items});
    data["scope"] = scope_for_view(ctx, &args_map);
    let envelope = success_envelope(data.clone(), &snap_label, snap_label != "live", started);
    let text = render_get_contract(&data);
    Ok(outcome(envelope, &data, text))
}

// ─── list_unresolved implementation ──────────────────────────────────

async fn run_list_unresolved(
    ctx: &McpContext<'_>,
    args_map: Map<String, Value>,
    started: Instant,
) -> Result<ToolOutcome, ToolOutcome> {
    let snap_label = snapshot_label(&args_map);
    let view = match resolve_view(ctx, &args_map, started).await? {
        ViewHandle::Empty(_) => {
            let mut data = json!({"items": [], "ambiguous": []});
            data["scope"] = scope_for_view(ctx, &args_map);
            let envelope =
                success_envelope(data.clone(), &snap_label, snap_label != "live", started);
            return Ok(outcome(envelope, &data, render_list_unresolved(&data)));
        }
        ViewHandle::Index { index, .. } => index,
    };
    let limit = parse_limit(&args_map, started)?;
    let cursor = parse_cursor(&args_map, started)?;
    let repo_filter = args_map
        .get("repo")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let service_filter = args_map
        .get("service")
        .and_then(|v| v.as_str())
        .map(|s| ServiceName(s.to_string()));

    let mut items: Vec<Value> = Vec::new();
    for (call_id, resolution) in &view.consumers {
        let Some(target) = &resolution.target else {
            continue;
        };
        let (reason, target_service) = match target {
            ConsumerTarget::Unresolved {
                reason,
                target_service,
            } => (*reason, target_service.clone()),
            _ => continue,
        };
        if let Some(ref repo) = repo_filter {
            if call_id.repo_id() != repo.as_str() {
                continue;
            }
        }
        if let Some(ref svc) = service_filter {
            if &resolution.service != svc {
                continue;
            }
        }
        let candidates = candidate_endpoints_for(&view, call_id, resolution);
        items.push(json!({
            "consumer": evidence_ref_from_call(call_id),
            "url_expr": url_expr_for(call_id),
            "method": method_label_for(call_id),
            "template": template_label_for(call_id),
            "host": host_label_for(call_id),
            "reason": unresolved_reason_label(reason),
            "target_service": target_service.as_ref().map(|s| s.0.clone()),
            "candidates": candidates,
        }));
    }

    let mut ambiguous: Vec<Value> = Vec::new();
    for (call_id, resolution) in &view.consumers {
        if let Some(ConsumerTarget::Binds { .. }) = &resolution.target {
            if resolution.bound_endpoints.len() > 1 {
                let mut cands: Vec<Value> = Vec::new();
                for ep in &resolution.bound_endpoints {
                    cands.push(json!({"service": ep.0 .0, "key": ep.1.to_string()}));
                }
                ambiguous.push(json!({
                    "consumer": evidence_ref_from_call(call_id),
                    "candidates": cands,
                }));
            }
        }
    }

    let key = |it: &Value| it["consumer"]["id"].as_str().unwrap_or("").to_string();
    if let Some(after) = cursor.as_ref().map(|c| c.after.clone()) {
        items.retain(|it| key(it) > after);
    }
    let fp = fingerprint(&args_map);
    let (page, next_cursor) = apply_limit(items, limit, key, &fp);

    let mut data = json!({
        "items": page,
        "ambiguous": ambiguous,
    });
    if let Some(tok) = next_cursor {
        data["cursor"] = json!(tok);
    }
    data["scope"] = scope_for_view(ctx, &args_map);
    let envelope = success_envelope(data.clone(), &snap_label, snap_label != "live", started);
    let text = render_list_unresolved(&data);
    Ok(outcome(envelope, &data, text))
}

// ─── check_binding implementation ─────────────────────────────────────

async fn run_check_binding(
    ctx: &McpContext<'_>,
    args_map: Map<String, Value>,
    started: Instant,
) -> Result<ToolOutcome, ToolOutcome> {
    let snap_label = snapshot_label(&args_map);
    let view = match resolve_view(ctx, &args_map, started).await? {
        ViewHandle::Empty(_) => {
            return Ok(error_outcome(
                "contract_not_found",
                "no contract index available for this snapshot",
                Some(json!({"endpoint": null})),
                &snap_label,
                started,
            ));
        }
        ViewHandle::Index { index, .. } => index,
    };

    let consumer_raw = args_map
        .get("consumer")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            error_outcome(
                "invalid_argument",
                "missing required argument: consumer",
                Some(json!({"arg": "consumer"})),
                &snap_label,
                started,
            )
        })?;
    let endpoint_raw = args_map.get("endpoint").ok_or_else(|| {
        error_outcome(
            "invalid_argument",
            "missing required argument: endpoint",
            Some(json!({"arg": "endpoint"})),
            &snap_label,
            started,
        )
    })?;

    let consumer_id = GlobalId::from_canonical(consumer_raw);
    if GlobalId::parse(consumer_id.as_str()).is_err() {
        return Err(error_outcome(
            "invalid_id",
            format!("malformed consumer GlobalId: {consumer_raw:?}"),
            Some(json!({"id": consumer_raw})),
            &snap_label,
            started,
        ));
    }
    let endpoint = parse_endpoint(endpoint_raw)?;
    let service_str = endpoint
        .get("service")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            error_outcome(
                "invalid_argument",
                "endpoint.service required",
                Some(json!({"arg": "endpoint.service"})),
                &snap_label,
                started,
            )
        })?;
    let key_str = endpoint
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            error_outcome(
                "invalid_argument",
                "endpoint.key required",
                Some(json!({"arg": "endpoint.key"})),
                &snap_label,
                started,
            )
        })?;
    let key = ContractKey::from_str(key_str).map_err(|e| {
        error_outcome(
            "invalid_argument",
            format!("malformed endpoint.key: {e}"),
            Some(json!({"arg": "endpoint.key", "reason": "malformed"})),
            &snap_label,
            started,
        )
    })?;

    let endpoint_id: EndpointId = (ServiceName(service_str.to_string()), key.clone());
    let endpoint_def = view.endpoints.get(&endpoint_id);

    let resolution = view.consumers.get(&consumer_id).cloned();
    let mut reasons: Vec<&'static str> = Vec::new();

    if resolution.is_none() {
        reasons.push("not_a_consumer");
    }
    let consumer_repo = consumer_id.repo_id().to_string();
    if let Some(res) = &resolution {
        if res.service.0 == service_str {
            reasons.push("same_service");
        }
        if let Some(ConsumerTarget::Binds { .. }) = &res.target {
            reasons.push("already_bound");
        }
    }
    let _ = (); // sentinel — keep the binding site stable
    let (consumer_method, consumer_template) = parse_consumer_call_shape(&consumer_id);
    let (provider_method, provider_template) = match &key {
        ContractKey::Http { method, template } => (method.clone(), template.clone()),
        _ => {
            return Err(error_outcome(
                "invalid_argument",
                "topic endpoints not supported in 0.9",
                Some(json!({"arg": "endpoint.key"})),
                &snap_label,
                started,
            ));
        }
    };

    let method_match = methods_compatible(&consumer_method, &provider_method);
    if !method_match {
        reasons.push("method_mismatch");
    }
    let (template_compatible, match_label) =
        templates_compatible(consumer_template.as_deref(), &provider_template);
    let template_match = match_label;
    if !template_compatible {
        reasons.push("template_mismatch");
    }
    let valid = reasons.is_empty() && endpoint_def.is_some();

    let data = if valid {
        let entry = render_bindings_entry(&consumer_repo, &consumer_id, &key, &endpoint_id);
        json!({
            "valid": true,
            "reasons": [],
            "method_match": method_match,
            "template_match": template_match,
            "bindings_entry": entry,
        })
    } else {
        json!({
            "valid": false,
            "reasons": reasons,
            "method_match": method_match,
            "template_match": template_match,
        })
    };

    let envelope = success_envelope(data.clone(), &snap_label, snap_label != "live", started);
    let text = render_check_binding(&data);
    Ok(outcome(envelope, &data, text))
}

fn parse_endpoint(v: &Value) -> Result<Value, ToolOutcome> {
    match v {
        Value::Object(_) => Ok(v.clone()),
        Value::String(s) => {
            let key = ContractKey::from_str(s).map_err(|e| ToolOutcome {
                structured: json!({}),
                text: format!("malformed endpoint: {e}"),
                is_error: true,
            })?;
            Ok(json!({"service": "", "key": key.to_string()}))
        }
        _ => Err(ToolOutcome {
            structured: json!({}),
            text: "endpoint must be an object {service, key}".to_string(),
            is_error: true,
        }),
    }
}

// ─── view resolution ─────────────────────────────────────────────────

/// Either an empty view (no `ContractIndex`, e.g. live with no
/// federation) or the resolved `ContractIndex` plus a `HoldGuard`
/// for residency.
pub enum ViewHandle {
    Empty(&'static str),
    Index {
        index: Arc<ContractIndex>,
        /// Hold on the residency slot; kept alive for the duration
        /// of the analysis. Dropped at the end of the handler.
        _hold: Option<Arc<()>>,
    },
}

pub async fn resolve_view(
    ctx: &McpContext<'_>,
    args_map: &Map<String, Value>,
    started: Instant,
) -> Result<ViewHandle, ToolOutcome> {
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
            return Ok(ViewHandle::Empty("federation disabled or empty"));
        };
        return Ok(ViewHandle::Index {
            index: idx,
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
        crate::federation::contracts::snapshots::manager::PrepareError::RepoNotRegistered {
            repo,
        } => error_outcome(
            "repo_not_registered",
            format!("repo {repo:?} not configured"),
            Some(json!({"repo": repo})),
            &snap_label,
            started,
        ),
        other => error_outcome(
            "invalid_argument",
            format!("{other:?}"),
            None,
            &snap_label,
            started,
        ),
    })?;
    // §10.5: 5_000 ms residency grace for analysis tools.
    let (fed, _guard) = mgr
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
    let ci = fed.contract_index.read().clone();
    let Some(idx) = ci else {
        return Ok(ViewHandle::Empty("snapshot has no contract index"));
    };
    Ok(ViewHandle::Index {
        index: idx,
        _hold: None,
    })
}

fn live_scope_when_live(ctx: &McpContext<'_>, args_map: &Map<String, Value>) -> Value {
    if snapshot_label(args_map) == "live" {
        ctx.federation.map(live_scope).unwrap_or_else(empty_scope)
    } else {
        empty_scope()
    }
}

fn scope_for_view(ctx: &McpContext<'_>, args_map: &Map<String, Value>) -> Value {
    // Snapshot view: scope comes from the snapshot record. The
    // §9.6 fields mirror the §10.8 wire shape.
    if snapshot_label(args_map) == "live" {
        return ctx.federation.map(live_scope).unwrap_or_else(empty_scope);
    }
    empty_scope()
}

fn empty_scope() -> Value {
    json!({
        "reviewed": [],
        "unreviewed": [],
        "configured_only": true,
    })
}

fn empty_outcome(scope: Value, _name: &str, snapshot: &str, started: Instant) -> ToolOutcome {
    let data = json!({"items": [], "scope": scope});
    let envelope = success_envelope(data.clone(), snapshot, snapshot != "live", started);
    let text = "# empty view\n".to_string();
    outcome(envelope, &data, text)
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

fn parse_limit(args_map: &Map<String, Value>, started: Instant) -> Result<usize, ToolOutcome> {
    let snap = snapshot_label(args_map);
    let raw = args_map.get("limit").and_then(|v| v.as_u64());
    match raw {
        None => Ok(100),
        Some(n) if (n as usize) > 1000 => Err(error_outcome(
            "range_too_large",
            format!("limit {n} exceeds max 1000"),
            Some(json!({"limit": n, "max": 1000, "requested": n})),
            &snap,
            started,
        )),
        Some(0) => Err(error_outcome(
            "invalid_argument",
            "limit must be >= 1",
            Some(json!({"arg": "limit"})),
            &snap,
            started,
        )),
        Some(n) => Ok(n as usize),
    }
}

fn parse_cursor(
    args_map: &Map<String, Value>,
    started: Instant,
) -> Result<Option<super::paging::CursorPayload>, ToolOutcome> {
    let snap = snapshot_label(args_map);
    let Some(token) = args_map.get("cursor").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    let cur = match decode_cursor(token) {
        Ok(c) => c,
        Err(_) => {
            return Err(error_outcome(
                "invalid_argument",
                "malformed cursor",
                Some(json!({"arg": "cursor", "reason": "malformed"})),
                &snap,
                started,
            ));
        }
    };
    let want = fingerprint(args_map);
    if cur.q != want {
        return Err(error_outcome(
            "invalid_argument",
            "cursor does not match the current arguments",
            Some(json!({"arg": "cursor", "reason": "cursor_mismatch"})),
            &snap,
            started,
        ));
    }
    Ok(Some(cur))
}

fn bound_consumers_count(idx: &ContractIndex, endpoint_id: &EndpointId) -> usize {
    idx.consumers
        .values()
        .filter(|r| match &r.target {
            Some(ConsumerTarget::Binds { .. }) => r.bound_endpoints.contains(endpoint_id),
            _ => false,
        })
        .count()
}

fn collect_consumers_for_endpoint(idx: &ContractIndex, endpoint_id: &EndpointId) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for (call_id, res) in &idx.consumers {
        if !res.bound_endpoints.contains(endpoint_id) {
            continue;
        }
        let (provenance, route_match) = match &res.target {
            Some(ConsumerTarget::Binds {
                provenance,
                route_match,
                ..
            }) => (provenance, *route_match),
            _ => continue,
        };
        let reads = collect_reads_for_consumer(idx, call_id);
        out.push(json!({
            "site": evidence_ref_from_call(call_id),
            "caller": caller_evidence(call_id),
            "binding": provenance_to_json(provenance),
            "match": route_match_label(route_match),
            "reads_complete": res.reads_complete,
            "reads": reads,
        }));
    }
    out.sort_by(|a, b| {
        let al = a["site"]["line"].as_u64().unwrap_or(0);
        let bl = b["site"]["line"].as_u64().unwrap_or(0);
        al.cmp(&bl)
    });
    out
}

fn collect_reads_for_consumer(idx: &ContractIndex, call_id: &GlobalId) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for ref_id in idx.field_refs.keys() {
        let Some(field) = bound_field_for_call(idx, ref_id, call_id) else {
            continue;
        };
        out.push(json!({
            "json_path": field.field_path.to_string(),
            "site": evidence_ref(ref_id, &ref_id.path().unwrap_or_default(), ref_id.line_start().unwrap_or(0)),
            "provenance": provenance_to_json(&field_from_bound_field(idx, field).unwrap_or_else(static_provenance_ref)),
        }));
    }
    out.sort_by(|a, b| {
        a["json_path"]
            .as_str()
            .unwrap_or("")
            .cmp(b["json_path"].as_str().unwrap_or(""))
    });
    out
}

fn static_provenance_ref() -> EdgeProvenance {
    EdgeProvenance::Static {
        source: crate::schema::StaticSource::TreeSitter,
    }
}

fn bound_field_for_call<'a>(
    idx: &'a ContractIndex,
    ref_id: &GlobalId,
    call_id: &GlobalId,
) -> Option<&'a crate::federation::contracts::index::BoundField> {
    // The FieldRef must `ReadsFrom` this call (§7.5); among its
    // bound fields prefer one whose endpoint this call actually
    // binds to, falling back to the first bound field.
    let fr = idx.field_refs.get(ref_id)?;
    if fr.call != call_id.as_str() {
        return None;
    }
    let call = idx.consumers.get(call_id)?;
    fr.bound_fields
        .iter()
        .find(|b| call.bound_endpoints.contains(&b.endpoint))
        .or_else(|| fr.bound_fields.first())
}

fn field_from_bound_field(
    _idx: &ContractIndex,
    b: &crate::federation::contracts::index::BoundField,
) -> Option<EdgeProvenance> {
    // §7.5: exact-path reads bind `Static` at the capped confidence;
    // suffix/ambiguous joins stay heuristic with the §7.5 detectors.
    if (b.confidence - 1.0).abs() < f32::EPSILON {
        Some(EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        })
    } else if (b.confidence - 0.6).abs() < f32::EPSILON {
        Some(EdgeProvenance::Heuristic {
            detector: "field_suffix".into(),
            confidence: b.confidence,
        })
    } else {
        Some(EdgeProvenance::Heuristic {
            detector: "ambiguous_field".into(),
            confidence: b.confidence,
        })
    }
}

fn evidence_ref(id: &GlobalId, path: &str, line: u32) -> Value {
    json!({
        "id": id.as_str(),
        "repo": id.repo_id(),
        "commit": "",
        "path": path,
        "line": line,
        "text": "",
    })
}

fn evidence_ref_from_call(call_id: &GlobalId) -> Value {
    json!({
        "id": call_id.as_str(),
        "repo": call_id.repo_id(),
        "commit": "",
        "path": call_id.path().unwrap_or_default(),
        "line": call_id.line_start().unwrap_or(0),
        "text": call_id.name().unwrap_or_default(),
    })
}

fn caller_evidence(call_id: &GlobalId) -> Value {
    json!({
        "id": call_id.as_str(),
        "name": call_id.name().unwrap_or_default(),
        "repo": call_id.repo_id(),
        "commit": "",
        "path": call_id.path().unwrap_or_default(),
        "line": call_id.line_start().unwrap_or(0),
        "text": "",
    })
}

fn url_expr_for(call_id: &GlobalId) -> String {
    call_id.name().unwrap_or_default()
}

fn method_label_for(call_id: &GlobalId) -> String {
    let n = call_id.name().unwrap_or_default();
    n.split_whitespace().next().unwrap_or("").to_string()
}

fn template_label_for(call_id: &GlobalId) -> Option<String> {
    let n = call_id.name().unwrap_or_default();
    n.split_whitespace().nth(1).map(|s| s.to_string())
}

fn host_label_for(_call_id: &GlobalId) -> Option<String> {
    None
}

fn unresolved_reason_label(r: UnresolvedReason) -> &'static str {
    match r {
        UnresolvedReason::NoRouteInService => "no_route_in_service",
        UnresolvedReason::NoMatch => "no_match",
        UnresolvedReason::Unnormalized => "no_route_in_service",
        UnresolvedReason::WrapperUnconfigured => "wrapper_unconfigured",
    }
}

fn route_match_label(r: RouteMatch) -> &'static str {
    match r {
        RouteMatch::Exact => "exact",
        RouteMatch::Pattern => "pattern",
        RouteMatch::PrefixStripped => "prefix_stripped",
    }
}

fn direction_label(d: Direction) -> &'static str {
    match d {
        Direction::Request => "request",
        Direction::Response => "response",
        Direction::Payload => "payload",
    }
}

fn type_desc_label(t: &crate::federation::contracts::model::TypeDesc) -> &'static str {
    use crate::federation::contracts::model::TypeDesc;
    match t {
        TypeDesc::String => "string",
        TypeDesc::Integer => "integer",
        TypeDesc::Number => "number",
        TypeDesc::Boolean => "boolean",
        TypeDesc::Object => "object",
        TypeDesc::Array(_) => "array",
        TypeDesc::Unknown => "unknown",
    }
}

fn provenance_to_json(p: &EdgeProvenance) -> Value {
    let (kind, confidence, detector, source) = match p {
        EdgeProvenance::Static { source } => ("static", 1.0_f32, None, Some(format!("{source:?}"))),
        EdgeProvenance::Heuristic {
            detector,
            confidence,
        } => ("heuristic", *confidence, Some(detector.clone()), None),
        EdgeProvenance::Runtime {
            trace_id,
            last_seen_unix,
        } => (
            "runtime",
            1.0_f32,
            Some(trace_id.clone()),
            Some(last_seen_unix.to_string()),
        ),
        EdgeProvenance::Confirmed { source } => ("confirmed", 1.0_f32, None, Some(source.clone())),
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

fn candidate_endpoints_for(
    view: &ContractIndex,
    call_id: &GlobalId,
    _resolution: &ConsumerResolution,
) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let (consumer_method, consumer_template) = parse_consumer_call_shape(call_id);
    for ep_id in view.endpoints.keys() {
        if let ContractKey::Http { method, template } = &ep_id.1 {
            let same_method = match (&consumer_method, method) {
                (
                    crate::federation::contracts::model::MethodSpec::Known(cm),
                    crate::federation::contracts::model::MethodSpec::Known(pm),
                ) => cm == pm || matches!(pm, crate::federation::contracts::model::HttpMethod::Any),
                (crate::federation::contracts::model::MethodSpec::Unknown, _) => true,
                _ => false,
            };
            if !same_method {
                continue;
            }
            let (ok, reason) = match consumer_template.as_deref() {
                Some(t) => {
                    let provider_method: crate::federation::contracts::model::HttpMethod =
                        match method {
                            MethodSpec::Known(pm) => *pm,
                            MethodSpec::Unknown => {
                                crate::federation::contracts::model::HttpMethod::Any
                            }
                        };
                    let r = match_route(consumer_method.clone(), t, provider_method, template);
                    if r.is_match() {
                        (true, "same_key")
                    } else {
                        (false, "pattern")
                    }
                }
                None => (true, "pattern"),
            };
            let _ = ok;
            if ok {
                out.push(json!({
                    "endpoint": {"service": ep_id.0 .0, "key": ep_id.1.to_string()},
                    "reason": reason,
                }));
            }
        }
    }
    out
}

fn parse_consumer_call_shape(
    call_id: &GlobalId,
) -> (
    crate::federation::contracts::model::MethodSpec,
    Option<String>,
) {
    let name = call_id.name().unwrap_or_default();
    let parts: Vec<&str> = name.split_whitespace().collect();
    let method = match parts.first().copied() {
        Some("GET") => crate::federation::contracts::model::MethodSpec::Known(
            crate::federation::contracts::model::HttpMethod::Get,
        ),
        Some("POST") => crate::federation::contracts::model::MethodSpec::Known(
            crate::federation::contracts::model::HttpMethod::Post,
        ),
        Some("PUT") => crate::federation::contracts::model::MethodSpec::Known(
            crate::federation::contracts::model::HttpMethod::Put,
        ),
        Some("PATCH") => crate::federation::contracts::model::MethodSpec::Known(
            crate::federation::contracts::model::HttpMethod::Patch,
        ),
        Some("DELETE") => crate::federation::contracts::model::MethodSpec::Known(
            crate::federation::contracts::model::HttpMethod::Delete,
        ),
        Some("HEAD") => crate::federation::contracts::model::MethodSpec::Known(
            crate::federation::contracts::model::HttpMethod::Head,
        ),
        Some("OPTIONS") => crate::federation::contracts::model::MethodSpec::Known(
            crate::federation::contracts::model::HttpMethod::Options,
        ),
        Some("ANY") => crate::federation::contracts::model::MethodSpec::Known(
            crate::federation::contracts::model::HttpMethod::Any,
        ),
        _ => crate::federation::contracts::model::MethodSpec::Unknown,
    };
    (method, parts.get(1).map(|s| s.to_string()))
}

fn methods_compatible(
    cm: &crate::federation::contracts::model::MethodSpec,
    pm: &crate::federation::contracts::model::MethodSpec,
) -> bool {
    use crate::federation::contracts::model::{HttpMethod, MethodSpec};
    match (cm, pm) {
        (MethodSpec::Known(a), MethodSpec::Known(b)) => {
            a == b || matches!(b, HttpMethod::Any) || matches!(a, HttpMethod::Any)
        }
        (MethodSpec::Unknown, _) | (_, MethodSpec::Unknown) => true,
    }
}

fn templates_compatible(consumer: Option<&str>, provider: &str) -> (bool, &'static str) {
    let Some(c) = consumer else {
        return (true, "pattern");
    };
    let r = match_route(
        crate::federation::contracts::model::MethodSpec::Known(
            crate::federation::contracts::model::HttpMethod::Any,
        ),
        c,
        crate::federation::contracts::model::HttpMethod::Any,
        provider,
    );
    if r.is_match() {
        (true, "exact")
    } else {
        (false, "none")
    }
}

fn render_bindings_entry(
    consumer_repo: &str,
    consumer_id: &GlobalId,
    key: &ContractKey,
    endpoint_id: &EndpointId,
) -> String {
    let symbol = consumer_id.name().unwrap_or_default();
    let path = consumer_id.path().unwrap_or_default();
    format!(
        "- consumer:\n    repo: {consumer_repo}\n    path: {path}\n    symbol: {symbol}\n    key: \"{key}\"\n  provider:\n    service: {svc}\n    key: \"{pkey}\"\n",
        svc = endpoint_id.0 .0,
        pkey = endpoint_id.1,
    )
}

// ─── renderers ────────────────────────────────────────────────────────

fn render_list_contracts(data: &Value) -> String {
    let items = data["items"].as_array().cloned().unwrap_or_default();
    let mut out = String::new();
    out.push_str(&format!("# Contracts ({} item(s))\n", items.len()));
    for it in &items {
        out.push_str(&format!(
            "- {} {} (providers: {}, bound_consumers: {})\n",
            it["endpoint"]["service"].as_str().unwrap_or(""),
            it["endpoint"]["key"].as_str().unwrap_or(""),
            it["providers"].as_array().map(|a| a.len()).unwrap_or(0),
            it["bound_consumers"].as_u64().unwrap_or(0),
        ));
    }
    out
}

fn render_get_contract(data: &Value) -> String {
    let items = data["items"].as_array().cloned().unwrap_or_default();
    let mut out = String::new();
    out.push_str(&format!("# Contract detail ({} match(es))\n", items.len()));
    for it in &items {
        out.push_str(&format!(
            "\n## {} {}\n",
            it["endpoint"]["service"].as_str().unwrap_or(""),
            it["endpoint"]["key"].as_str().unwrap_or(""),
        ));
        let schemas = it["schemas"].as_array().cloned().unwrap_or_default();
        for s in &schemas {
            out.push_str(&format!(
                "- {} ({} fields)\n",
                s["direction"].as_str().unwrap_or(""),
                s["fields"].as_array().map(|a| a.len()).unwrap_or(0),
            ));
        }
        let consumers = it["consumers"].as_array().cloned().unwrap_or_default();
        out.push_str(&format!("- {} consumers\n", consumers.len()));
    }
    out
}

fn render_list_unresolved(data: &Value) -> String {
    let items = data["items"].as_array().cloned().unwrap_or_default();
    let ambiguous = data["ambiguous"].as_array().cloned().unwrap_or_default();
    let mut out = String::new();
    out.push_str(&format!(
        "# Unresolved consumers ({} item(s), {} ambiguous)\n",
        items.len(),
        ambiguous.len()
    ));
    for it in &items {
        out.push_str(&format!(
            "- {} (reason: {})\n",
            it["consumer"]["id"].as_str().unwrap_or(""),
            it["reason"].as_str().unwrap_or(""),
        ));
    }
    out
}

fn render_check_binding(data: &Value) -> String {
    let valid = data["valid"].as_bool().unwrap_or(false);
    let mut out = String::new();
    out.push_str(if valid {
        "# check_binding: VALID\n"
    } else {
        "# check_binding: INVALID\n"
    });
    let reasons = data["reasons"].as_array().cloned().unwrap_or_default();
    if !reasons.is_empty() {
        let list: Vec<String> = reasons
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        out.push_str(&format!("Reasons: {}\n", list.join(", ")));
    }
    if let Some(s) = data["bindings_entry"].as_str() {
        out.push_str(&format!("\n{}\n", s));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::contracts::index::EndpointId;
    use crate::federation::contracts::model::{HttpMethod, MethodSpec};
    use crate::federation::repo_id::RepoId;

    #[test]
    fn parse_consumer_call_shape_extracts_method_and_template() {
        let id = GlobalId::new(
            &RepoId::new("orders").unwrap(),
            crate::schema::NodeType::HttpClientCall,
            "src/main.py",
            "GET /api/orders/{}",
            Some(20),
        );
        let gid = GlobalId::from_canonical(id.as_str());
        let (m, t) = parse_consumer_call_shape(&gid);
        assert!(matches!(
            m,
            MethodSpec::Known(crate::federation::contracts::model::HttpMethod::Get)
        ));
        assert_eq!(t.as_deref(), Some("/api/orders/{}"));
    }

    #[test]
    fn methods_compatible_handles_any() {
        let g = MethodSpec::Known(HttpMethod::Get);
        let a = MethodSpec::Known(HttpMethod::Any);
        let u = MethodSpec::Unknown;
        assert!(methods_compatible(&g, &a));
        assert!(methods_compatible(&a, &g));
        assert!(methods_compatible(&u, &g));
        let p = MethodSpec::Known(HttpMethod::Post);
        assert!(!methods_compatible(&g, &p));
    }

    #[test]
    fn templates_compatible_returns_exact_when_paths_match() {
        let (ok, label) = templates_compatible(Some("/api/orders/{}"), "/api/orders/{}");
        assert!(ok);
        assert_eq!(label, "exact");
    }

    #[test]
    fn templates_compatible_returns_none_on_mismatch() {
        let (ok, label) = templates_compatible(Some("/api/users/{}"), "/api/orders/{}");
        assert!(!ok);
        assert_eq!(label, "none");
    }

    #[test]
    fn route_match_label_maps_enum() {
        assert_eq!(route_match_label(RouteMatch::Exact), "exact");
        assert_eq!(route_match_label(RouteMatch::Pattern), "pattern");
        assert_eq!(
            route_match_label(RouteMatch::PrefixStripped),
            "prefix_stripped"
        );
    }

    #[test]
    fn direction_label_maps_enum() {
        assert_eq!(direction_label(Direction::Request), "request");
        assert_eq!(direction_label(Direction::Response), "response");
        assert_eq!(direction_label(Direction::Payload), "payload");
    }

    #[test]
    fn unresolved_reason_label_maps_enum() {
        assert_eq!(
            unresolved_reason_label(UnresolvedReason::NoRouteInService),
            "no_route_in_service"
        );
        assert_eq!(
            unresolved_reason_label(UnresolvedReason::NoMatch),
            "no_match"
        );
        assert_eq!(
            unresolved_reason_label(UnresolvedReason::Unnormalized),
            "no_route_in_service"
        );
        assert_eq!(
            unresolved_reason_label(UnresolvedReason::WrapperUnconfigured),
            "wrapper_unconfigured"
        );
    }

    #[test]
    fn render_bindings_entry_yamls_consumer_provider_pair() {
        let consumer =
            GlobalId::from_canonical("billing:HttpClientCall:src/main.py:fetch_order:20");
        let key = ContractKey::Http {
            method: MethodSpec::Known(HttpMethod::Get),
            template: "/api/orders/{}".into(),
        };
        let ep_id: EndpointId = (ServiceName("orders".into()), key.clone());
        let entry = render_bindings_entry("billing", &consumer, &key, &ep_id);
        assert!(entry.contains("repo: billing"));
        assert!(entry.contains("symbol: fetch_order"));
        assert!(entry.contains("service: orders"));
        assert!(entry.contains("key:"));
    }

    #[test]
    fn empty_outcome_carries_scope() {
        let started = Instant::now();
        let scope = json!({"reviewed": [], "unreviewed": [], "configured_only": true});
        let o = empty_outcome(scope.clone(), "list_contracts", "live", started);
        assert_eq!(o.structured["data"]["scope"], scope);
        assert!(!o.is_error);
    }

    #[test]
    fn parse_limit_rejects_over_max() {
        let mut m = Map::new();
        m.insert("limit".to_string(), json!(1001));
        let started = Instant::now();
        let o = parse_limit(&m, started).unwrap_err();
        assert!(o.is_error);
        assert_eq!(o.structured["error"]["code"], json!("range_too_large"));
    }

    #[test]
    fn parse_cursor_rejects_mismatch() {
        let token = super::super::paging::encode_cursor("orders", "deadbeef");
        let mut m = Map::new();
        m.insert("snapshot".to_string(), json!("live"));
        m.insert("service".to_string(), json!("orders"));
        m.insert("cursor".to_string(), json!(token));
        let started = Instant::now();
        let o = parse_cursor(&m, started).unwrap_err();
        assert_eq!(
            o.structured["error"]["details"]["reason"],
            json!("cursor_mismatch")
        );
    }
}
