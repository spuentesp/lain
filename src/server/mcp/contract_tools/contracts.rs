//! Contract tools: `list_contracts`, `get_contract`, `list_unresolved`,
//! `check_binding` (`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §12 + §13).
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

use super::envelope::{
    check_api_version, error_outcome, outcome, success_envelope, success_envelope_with_view,
};
use super::paging::{apply_limit, decode_cursor, fingerprint};
use super::view::{resolve_view, snapshot_label as view_snapshot_label};
use super::{ContractToolEntry, ContractToolFuture, ToolOutcome};
use crate::federation::contracts::index::{
    ConsumerResolution, ConsumerTarget, ContractIndex, Endpoint, EndpointId, UnresolvedReason,
};
use crate::federation::contracts::key_compat::{
    keys_compatible, methods_compatible, templates_compatible,
};
use crate::federation::contracts::model::MethodSpec;
use crate::federation::contracts::model::{ContractKey, Direction, ServiceName};
use crate::federation::contracts::route_match::{match_route, MatchOutcome};
use crate::federation::graph_backend::GraphBackend;
use crate::federation::repo_id::GlobalId;
use crate::schema::{EdgeProvenance, RouteMatch};
use crate::server::mcp::handler::McpContext;
use crate::server::sensors::util::GRAPHQL_CALL_PREFIX;
use serde_json::{json, Map, Value};
use std::str::FromStr;
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
    let resolved = resolve_view(ctx, &args_map, started).await?;
    let Some(view) = resolved.index().cloned() else {
        return Ok(empty_outcome(
            resolved.scope().clone(),
            "list_contracts",
            &snap_label,
            started,
        ));
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

    // Reject an unknown kind instead of returning an empty page. An
    // empty page reads as "no such contract here", which is the
    // `absent` vs `not analysed` conflation the coverage ledger exists
    // to prevent. Keep this list in step with `ContractKey::kind()`
    // and the `kind` enum in `schemas/list_contracts.in.json`.
    const VALID_KINDS: &[&str] = &["http", "topic", "rpc", "graphql", "websocket", "table"];
    if let Some(ref k) = kind_filter {
        if !VALID_KINDS.contains(&k.as_str()) {
            return Err(error_outcome(
                "invalid_argument",
                format!("unknown kind {k:?}"),
                Some(json!({
                    "arg": "kind",
                    "reason": "unknown",
                    "allowed": VALID_KINDS,
                })),
                &view_snapshot_label(&args_map),
                started,
            ));
        }
    }

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
            if k != endpoint_id.1.kind() {
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
    data["scope"] = resolved.scope().clone();
    let envelope = success_envelope_with_view(
        data.clone(),
        resolved.label(),
        resolved.reproducible(),
        resolved.view_info(),
        started,
    );
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
    let resolved = resolve_view(ctx, &args_map, started).await?;
    let Some(view) = resolved.index().cloned() else {
        return Ok(error_outcome(
            "contract_not_found",
            "no contract index available for this snapshot",
            Some(json!({"endpoint": null})),
            &snap_label,
            started,
        ));
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
    data["scope"] = resolved.scope().clone();
    let envelope = success_envelope_with_view(
        data.clone(),
        resolved.label(),
        resolved.reproducible(),
        resolved.view_info(),
        started,
    );
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
    let resolved = resolve_view(ctx, &args_map, started).await?;
    let Some(view) = resolved.index().cloned() else {
        let mut data = json!({"items": [], "ambiguous": []});
        data["scope"] = resolved.scope().clone();
        let envelope = success_envelope(data.clone(), &snap_label, snap_label != "live", started);
        return Ok(outcome(envelope, &data, render_list_unresolved(&data)));
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
    for (call_id, resolution) in view.consumer_resolutions() {
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
    for (call_id, resolution) in view.consumer_resolutions() {
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
    data["scope"] = resolved.scope().clone();
    let envelope = success_envelope_with_view(
        data.clone(),
        resolved.label(),
        resolved.reproducible(),
        resolved.view_info(),
        started,
    );
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
    let resolved = resolve_view(ctx, &args_map, started).await?;
    let Some(view) = resolved.index().cloned() else {
        return Ok(error_outcome(
            "contract_not_found",
            "no contract index available for this snapshot",
            Some(json!({"endpoint": null})),
            &snap_label,
            started,
        ));
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

    let resolution = view.consumer(&consumer_id).cloned();
    let mut reasons: Vec<&'static str> = Vec::new();

    if resolution.is_none() {
        reasons.push("not_a_consumer");
    }
    if let Some(res) = &resolution {
        if res.service.0 == service_str {
            reasons.push("same_service");
        }
        if let Some(ConsumerTarget::Binds { .. }) = &res.target {
            // Confirming the endpoint the consumer is already bound
            // to is idempotent — the joiner records the entry with
            // `Confirmed` provenance. The conflict is re-pointing at
            // a *different* endpoint.
            if !res.bound_endpoints.iter().any(|e| e == &endpoint_id) {
                reasons.push("already_bound");
            }
        }
    }
    let _ = (); // sentinel — keep the binding site stable
    let fact_keys = consumer_fact_keys(resolved.backend().as_ref(), &consumer_id);
    // Cross-kind guard for fact-keyed consumers (Topic/Rpc/
    // WebSocket/Table) against an `http:` endpoint key: the HTTP arm
    // below treats a call-site name with no method/template as a
    // pattern match (`Unknown` / no template), so an ill-formed
    // cross-kind pair would otherwise validate vacuously. The reason
    // is the existing `method_mismatch` (the wrong-shape axis) — no
    // new reason value. `graphql:` endpoint keys are already
    // rejected by the GraphQL arm's name parse below.
    //
    // The guard fires inside the HTTP arm, where it can return the
    // verdict fields that agree with the reason — `(false, "none")`,
    // the fact-keyed arm's cross-kind shape. Pushed before the match
    // (as this used to be), the vacuous HTTP comparison reported
    // `method_match: true, template_match: "pattern"` beside a
    // `method_mismatch` reason.
    let cross_kind_http_pair = matches!(key, ContractKey::Http { .. })
        && fact_keys
            .iter()
            .any(|k| matches!(k.kind(), "topic" | "rpc" | "websocket" | "table"))
        && !fact_keys.iter().any(|k| k.kind() == key.kind());
    let (consumer_method, consumer_template) = parse_consumer_call_shape(&consumer_id);
    let (method_match, template_match) = match &key {
        ContractKey::Http { method, template } => {
            if consumer_id
                .name()
                .is_some_and(|n| n.starts_with(GRAPHQL_CALL_PREFIX))
            {
                // Key-kind mismatch: a `graphql-call:{op}:{field}`
                // node carries no HTTP method/template, so the HTTP
                // comparison would be vacuous (`Unknown`/`None` always
                // "match"). Report the GraphQL pair as unmatched
                // instead of a misleading `template_mismatch`.
                reasons.push("op_mismatch");
                reasons.push("field_mismatch");
                (false, "none")
            } else if cross_kind_http_pair {
                // Cross-kind guard (see `cross_kind_http_pair` above):
                // the reason is `method_mismatch` and the verdict
                // fields say the same — `(false, "none")`, matching the
                // fact-keyed arm's cross-kind shape — instead of the
                // vacuous `Unknown`/no-template comparison's
                // `(true, "pattern")`.
                reasons.push("method_mismatch");
                (false, "none")
            } else {
                let mm = methods_compatible(&consumer_method, method);
                if !mm {
                    reasons.push("method_mismatch");
                }
                let (tc, label) = templates_compatible(consumer_template.as_deref(), template);
                if !tc {
                    reasons.push("template_mismatch");
                }
                (mm, label)
            }
        }
        ContractKey::Graphql { op, field } => {
            // GraphQL consumers are synthetic nodes named
            // `graphql-call:{op}:{field}`; `parse_consumer_call_shape`
            // cannot see them (no whitespace in the name). Compare
            // `(op, field)` against the name directly and reuse the
            // same verdict fields: `method_match` is op equality,
            // `template_match` is `exact`/`none` on field equality.
            // A non-GraphQL consumer name falls through the prefix
            // strip to empty parts and mismatches cleanly on both.
            let name = consumer_id.name().unwrap_or_default();
            let rest = name.strip_prefix(GRAPHQL_CALL_PREFIX).unwrap_or("");
            let (cop, cfield) = rest.split_once(':').unwrap_or((rest, ""));
            let op_ok = cop == op.to_string();
            let field_ok = cfield == field;
            if !op_ok {
                reasons.push("op_mismatch");
            }
            if !field_ok {
                reasons.push("field_mismatch");
            }
            (op_ok, if field_ok { "exact" } else { "none" })
        }
        key @ (ContractKey::Topic { .. }
        | ContractKey::Rpc { .. }
        | ContractKey::WebSocket { .. }
        | ContractKey::Table { .. }) => {
            // Fact-keyed kinds: the consumer's key comes from its
            // contract fact(s) (`consumer_fact_keys`), not its name.
            // A well-formed pair is same-kind + `keys_compatible`'s
            // equality semantics (broker+name / system+service+method
            // / route / name). Reason mapping reuses the HTTP labels:
            // cross-kind → `method_mismatch` (the shape axis),
            // same-kind key mismatch → `template_mismatch` (the leaf
            // axis). A graphql-call consumer carries no such key —
            // report the GraphQL pair as unmatched (the HTTP arm's
            // rule) instead of misleading labels.
            if consumer_id
                .name()
                .is_some_and(|n| n.starts_with(GRAPHQL_CALL_PREFIX))
            {
                reasons.push("op_mismatch");
                reasons.push("field_mismatch");
                (false, "none")
            } else {
                let same_kind: Vec<&ContractKey> = fact_keys
                    .iter()
                    .filter(|k| k.kind() == key.kind())
                    .collect();
                if same_kind.is_empty() {
                    reasons.push("method_mismatch");
                    (false, "none")
                } else if same_kind.iter().any(|ck| keys_compatible(ck, key)) {
                    (true, "exact")
                } else {
                    reasons.push("template_mismatch");
                    (true, "none")
                }
            }
        }
    };
    // Emit the selector the confirmed joiner actually uses: the node's
    // enclosing symbol and its own fact key, which can differ from the
    // provider key for an HTTP template bridge. A fully dynamic consumer
    // with no fact key cannot be represented by the existing bindings
    // format and must not be advertised as a persistable confirmation.
    let entry = render_bindings_entry(resolved.backend().as_ref(), &consumer_id, &endpoint_id)
        .map_err(|e| {
            error_outcome(
                "internal",
                format!("serialize bindings_entry: {e}"),
                None,
                &snap_label,
                started,
            )
        })?;
    if entry.is_none() && reasons.is_empty() {
        reasons.push("not_a_consumer");
    }
    let valid = reasons.is_empty() && endpoint_def.is_some();

    let data = if valid {
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

    let envelope = success_envelope_with_view(
        data.clone(),
        resolved.label(),
        resolved.reproducible(),
        resolved.view_info(),
        started,
    );
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

pub(crate) fn snapshot_label(args_map: &Map<String, Value>) -> String {
    view_snapshot_label(args_map)
}

pub(crate) fn parse_limit(
    args_map: &Map<String, Value>,
    started: Instant,
) -> Result<usize, ToolOutcome> {
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

pub(crate) fn parse_cursor(
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
    idx.consumer_resolutions()
        .map(|(_, r)| r)
        .filter(|r| match &r.target {
            Some(ConsumerTarget::Binds { .. }) => r.bound_endpoints.contains(endpoint_id),
            _ => false,
        })
        .count()
}

fn collect_consumers_for_endpoint(idx: &ContractIndex, endpoint_id: &EndpointId) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for (call_id, res) in idx.consumer_resolutions() {
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
    let call = idx.consumer(call_id)?;
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

/// Render the wire-form label for a `model::UnresolvedReason`.
/// `pub(crate)` so `services.rs` (the per-service `list_unresolved`
/// envelope) can render the same labels — Phase A review §S1
/// collapsed the two parallel enums, and the wire-form table moves
/// to a single function.
pub(crate) fn unresolved_reason_label(r: UnresolvedReason) -> &'static str {
    use crate::federation::contracts::model::UnresolvedReason::*;
    match r {
        // Joinder verdict (Phase A review §S1 — unified model).
        NoRouteInService => "no_route_in_service",
        NoMatch => "no_match",
        Unnormalized => "no_route_in_service",
        WrapperUnconfigured => "wrapper_unconfigured",
        EnvUnmapped => "env_unmapped",
        EnvAmbiguous => "env_ambiguous",
        RpcStubUnknown => "rpc_stub_unknown",
        GraphqlNoOp => "graphql_no_op",
        // Sensor ledger bucket — collapsed into the same enum per
        // the Phase A review §S1 unification. The labels below
        // were the legacy `coverage::UnresolvedReason` strings.
        DynamicUrl => "dynamic_url",
        BaseUnknown => "base_unknown",
        DynamicTopic => "dynamic_topic",
        ExternalRef => "external_ref",
        DynamicSql => "dynamic_sql",
        OrmDynamicQuery => "orm_dynamic_query",
        ExternalSchemaRef => "external_schema_ref",
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
    resolution: &ConsumerResolution,
) -> Vec<Value> {
    // §9.7 gating: a `target_service` the resolver named (HTTP
    // `no_route_in_service`, GraphQL `route_owner`) means the
    // consumer could match only that service's endpoints; without
    // one the whole index is in play. The gate is a filter over the
    // existing listing rules, so `BTreeMap` key order is unchanged.
    let target_service: Option<&ServiceName> = match &resolution.target {
        Some(ConsumerTarget::Unresolved { target_service, .. }) => target_service.as_ref(),
        _ => None,
    };
    let in_target = |ep_id: &EndpointId| match target_service {
        Some(ts) => &ep_id.0 == ts,
        None => true,
    };
    // GraphQL consumers (`graphql-call:{op}:{field}`) carry no HTTP
    // method/template: `parse_consumer_call_shape` reads their name
    // as `Unknown`/no template, so the HTTP filter below would admit
    // every `ContractKey::Http` endpoint (pure noise). Their
    // candidates are the GraphQL endpoints in the index instead:
    // the same-`op` set when non-empty (a `query` consumer's
    // candidates are query endpoints), else every GraphQL endpoint.
    // HTTP endpoints are never listed. Reason labels stay on the
    // wire vocabulary (`list_unresolved.out.json`): `same_key` for an
    // exact `(op, field)` pair, `pattern` for any other GraphQL
    // candidate. Ordering is the index's `BTreeMap` key order.
    if call_id
        .name()
        .is_some_and(|n| n.starts_with(GRAPHQL_CALL_PREFIX))
    {
        let name = call_id.name().unwrap_or_default();
        let rest = name.strip_prefix(GRAPHQL_CALL_PREFIX).unwrap_or("");
        let (cop, cfield) = rest.split_once(':').unwrap_or((rest, ""));
        let same_op_present = view.endpoints.keys().any(|ep_id| {
            in_target(ep_id)
                && matches!(&ep_id.1, ContractKey::Graphql { op, .. } if op.to_string() == cop)
        });
        let mut out: Vec<Value> = Vec::new();
        for ep_id in view.endpoints.keys() {
            if !in_target(ep_id) {
                continue;
            }
            if let ContractKey::Graphql { op, field } = &ep_id.1 {
                let same_op = op.to_string() == cop;
                if same_op_present && !same_op {
                    continue;
                }
                let reason = if same_op && field == cfield {
                    "same_key"
                } else {
                    "pattern"
                };
                out.push(json!({
                    "endpoint": {"service": ep_id.0 .0, "key": ep_id.1.to_string()},
                    "reason": reason,
                }));
            }
        }
        return out;
    }

    let mut out: Vec<Value> = Vec::new();
    let (consumer_method, consumer_template) = parse_consumer_call_shape(call_id);
    for ep_id in view.endpoints.keys() {
        if !in_target(ep_id) {
            continue;
        }
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
                    match match_route(consumer_method.clone(), t, provider_method, template) {
                        MatchOutcome::Match(detail) => {
                            // Candidate labels reuse the §7.4
                            // classifier (`match_route`'s
                            // `RouteMatch` kind — the taxonomy
                            // `route_match_label` renders as
                            // `exact|pattern|prefix_stripped`):
                            // `same_key` only for a truly equal key,
                            // `prefix_stripped` when the match needed
                            // the matcher's gateway-prefix strip,
                            // `pattern` for any other route match.
                            let keys_identical =
                                &consumer_method == method && t == template.as_str();
                            let reason = if keys_identical {
                                "same_key"
                            } else if matches!(detail.kind, RouteMatch::PrefixStripped) {
                                "prefix_stripped"
                            } else {
                                "pattern"
                            };
                            (true, reason)
                        }
                        MatchOutcome::NoMatch => (false, "pattern"),
                    }
                }
                None => (true, "pattern"),
            };
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

/// The [`ContractKey`]s a consumer node names, taken from its
/// contract facts. The HTTP/GraphQL arms read the key from the
/// call-site name (`GET /path`, `graphql-call:{op}:{field}`); the
/// fact-keyed kinds (Topic/Rpc/WebSocket/Table) are ordinary
/// symbols whose key lives in the fact (`TopicConsumerFact`, …), so
/// `check_binding` has to open the node. Empty when the node is
/// gone or carries no consumer fact.
fn consumer_fact_keys(backend: &dyn GraphBackend, id: &GlobalId) -> Vec<ContractKey> {
    let Ok(Some(node)) = backend.get_node(id.as_str()) else {
        return Vec::new();
    };
    node.contract
        .iter()
        .flat_map(|f| f.consumer_keys())
        .collect()
}

fn parse_consumer_call_shape(
    call_id: &GlobalId,
) -> (
    crate::federation::contracts::model::MethodSpec,
    Option<String>,
) {
    let name = call_id.name().unwrap_or_default();
    let parts: Vec<&str> = name.split_whitespace().collect();
    let method = parts
        .first()
        .and_then(|v| crate::federation::contracts::model::HttpMethod::parse(v))
        .map_or(
            crate::federation::contracts::model::MethodSpec::Unknown,
            crate::federation::contracts::model::MethodSpec::Known,
        );
    (method, parts.get(1).map(|s| s.to_string()))
}

fn render_bindings_entry(
    backend: &dyn GraphBackend,
    consumer_id: &GlobalId,
    endpoint_id: &EndpointId,
) -> Result<Option<String>, serde_yaml::Error> {
    use crate::federation::contracts::config::{
        ConfirmedBinding, ConfirmedBindingConsumer, ConfirmedBindingProvider,
    };

    let Ok(Some(node)) = backend.get_node(consumer_id.as_str()) else {
        return Ok(None);
    };
    let keys: Vec<_> = node
        .contract
        .iter()
        .flat_map(|fact| fact.consumer_keys())
        .collect();
    let Some(key) = keys
        .iter()
        .find(|key| *key == &endpoint_id.1)
        .or_else(|| keys.iter().find(|key| keys_compatible(key, &endpoint_id.1)))
    else {
        return Ok(None);
    };
    let binding = ConfirmedBinding {
        consumer: ConfirmedBindingConsumer {
            repo: consumer_id.repo_id().to_string(),
            path: node.path,
            symbol: node.container.unwrap_or(node.name),
            key: key.to_string(),
        },
        provider: ConfirmedBindingProvider {
            service: endpoint_id.0 .0.clone(),
            key: endpoint_id.1.to_string(),
        },
    };
    // Serialize the existing config type so YAML-significant source
    // paths and symbols round-trip without changing the selector.
    serde_yaml::to_string(&vec![binding]).map(Some)
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
    use crate::federation::contracts::model::{GraphqlOp, HttpMethod, MethodSpec};
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
    fn render_bindings_entry_uses_actual_consumer_key_and_container() {
        use crate::federation::contracts::config::ConfirmedBinding;
        use crate::federation::contracts::model::{
            CallVia, ConsumerFact, ContractFact, HostPart, NormalizedUrl,
        };
        use crate::federation::graph_backend::PetgraphBackend;
        use crate::schema::{GraphNode, NodeType};

        let dir = tempfile::tempdir().unwrap();
        let backend = PetgraphBackend::new(dir.path()).unwrap();
        let consumer = GlobalId::new(
            &RepoId::new("billing").unwrap(),
            NodeType::HttpClientCall,
            "src/main: billing.py",
            "GET /v1/api/orders/{}",
            Some(20),
        );
        let mut node = GraphNode::new(
            NodeType::HttpClientCall,
            "GET /v1/api/orders/{}".into(),
            "src/main: billing.py".into(),
        );
        node.id = consumer.as_str().into();
        node.container = Some("fetch: order #1".into());
        node.contract.push(ContractFact::Consumer(ConsumerFact {
            method: MethodSpec::Known(HttpMethod::Get),
            url: NormalizedUrl {
                host: HostPart::None,
                template: Some("/v1/api/orders/{}".into()),
            },
            via: CallVia::Library {
                name: "requests".into(),
            },
            url_expr: "upstream".into(),
            reads_complete: true,
        }));
        backend.upsert_node(node).unwrap();
        let ep_id: EndpointId = (
            ServiceName("orders".into()),
            ContractKey::from_str("http:GET /api/orders/{}").unwrap(),
        );
        let entry = render_bindings_entry(&backend, &consumer, &ep_id)
            .unwrap()
            .unwrap();
        let bindings: Vec<ConfirmedBinding> = serde_yaml::from_str(&entry).unwrap();
        assert_eq!(bindings[0].consumer.repo, "billing");
        assert_eq!(bindings[0].consumer.path, "src/main: billing.py");
        assert_eq!(bindings[0].consumer.symbol, "fetch: order #1");
        assert_eq!(bindings[0].consumer.key, "http:GET /v1/api/orders/{}");
        assert_eq!(bindings[0].provider.key, "http:GET /api/orders/{}");
    }

    #[test]
    fn render_bindings_entry_refuses_missing_or_unkeyed_consumer() {
        use crate::federation::graph_backend::PetgraphBackend;
        use crate::schema::{GraphNode, NodeType};
        let dir = tempfile::tempdir().unwrap();
        let backend = PetgraphBackend::new(dir.path()).unwrap();
        let consumer = GlobalId::new(
            &RepoId::new("billing").unwrap(),
            NodeType::Function,
            "src/main.py",
            "fetch_order",
            Some(20),
        );
        let endpoint = (
            ServiceName("orders".into()),
            ContractKey::from_str("rpc:orders/GetOrder").unwrap(),
        );
        assert!(render_bindings_entry(&backend, &consumer, &endpoint)
            .unwrap()
            .is_none());
        let mut node = GraphNode::new(
            NodeType::Function,
            "fetch_order".into(),
            "src/main.py".into(),
        );
        node.id = consumer.as_str().into();
        backend.upsert_node(node).unwrap();
        assert!(render_bindings_entry(&backend, &consumer, &endpoint)
            .unwrap()
            .is_none());
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

    fn graphql_key(op: GraphqlOp, field: &str) -> ContractKey {
        ContractKey::Graphql {
            op,
            field: field.to_string(),
        }
    }

    fn http_key(method: HttpMethod, template: &str) -> ContractKey {
        ContractKey::Http {
            method: MethodSpec::Known(method),
            template: template.to_string(),
        }
    }

    fn insert_ep(view: &mut ContractIndex, service: &str, key: ContractKey) {
        let id: EndpointId = (ServiceName(service.to_string()), key);
        view.endpoints.insert(
            id.clone(),
            Endpoint {
                id,
                method: HttpMethod::Any,
                template: String::new(),
                providers: Vec::new(),
                schemas: Default::default(),
            },
        );
    }

    fn call_id(repo: &str, name: &str) -> GlobalId {
        let id = GlobalId::new(
            &RepoId::new(repo).unwrap(),
            crate::schema::NodeType::Function,
            "src/calls.ts",
            name,
            Some(10),
        );
        GlobalId::from_canonical(id.as_str())
    }

    fn candidate_triples(view: &ContractIndex, id: &GlobalId) -> Vec<(String, String, String)> {
        candidate_triples_for(view, id, None)
    }

    fn candidate_triples_for(
        view: &ContractIndex,
        id: &GlobalId,
        target_service: Option<&ServiceName>,
    ) -> Vec<(String, String, String)> {
        let res = ConsumerResolution {
            call_id: id.clone(),
            service: ServiceName("storefront".to_string()),
            target: Some(ConsumerTarget::Unresolved {
                reason: UnresolvedReason::NoMatch,
                target_service: target_service.cloned(),
            }),
            bound_endpoints: Vec::new(),
            reads_complete: true,
        };
        candidate_endpoints_for(view, id, &res)
            .into_iter()
            .map(|c| {
                (
                    c["endpoint"]["service"].as_str().unwrap().to_string(),
                    c["endpoint"]["key"].as_str().unwrap().to_string(),
                    c["reason"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    /// A `graphql-call:{op}:{field}` consumer must see the GraphQL
    /// endpoints in the index — never the HTTP endpoints the
    /// method/template filter used to admit wholesale (the consumer
    /// name has no whitespace, so it parsed as `Unknown`/no template
    /// and matched every `ContractKey::Http`).
    #[test]
    fn graphql_consumer_candidates_are_graphql_endpoints_only() {
        let mut view = ContractIndex::default();
        insert_ep(
            &mut view,
            "saleor-api",
            graphql_key(GraphqlOp::Query, "product"),
        );
        insert_ep(
            &mut view,
            "saleor-api",
            graphql_key(GraphqlOp::Query, "somethingElse"),
        );
        insert_ep(
            &mut view,
            "saleor-api",
            graphql_key(GraphqlOp::Mutation, "updateProduct"),
        );
        insert_ep(
            &mut view,
            "saleor-api",
            http_key(HttpMethod::Post, "/graphql"),
        );
        insert_ep(
            &mut view,
            "orders",
            http_key(HttpMethod::Get, "/api/orders/{}"),
        );

        let gid = call_id("storefront", "graphql-call:query:product");
        let got = candidate_triples(&view, &gid);
        assert!(!got.is_empty(), "graphql endpoints must be candidates");
        for (svc, key, _) in &got {
            assert!(
                key.starts_with("graphql:"),
                "graphql consumer must not see http endpoints: {svc} {key}"
            );
        }
        let keys: Vec<&str> = got.iter().map(|(_, k, _)| k.as_str()).collect();
        assert!(
            keys.iter().any(|k| *k == "graphql:query:product"),
            "the consumer's own (op, field) endpoint must be a candidate: {keys:?}"
        );
    }

    /// Same-`op` GraphQL endpoints are the candidates while any exist
    /// (a `query` consumer's candidates are query endpoints); with no
    /// same-op endpoint the consumer falls back to every GraphQL
    /// endpoint. Reason labels stay on the wire vocabulary: `same_key`
    /// for an exact `(op, field)` pair, `pattern` otherwise.
    #[test]
    fn graphql_consumer_candidates_prefer_same_op() {
        let mut view = ContractIndex::default();
        insert_ep(
            &mut view,
            "saleor-api",
            graphql_key(GraphqlOp::Query, "product"),
        );
        insert_ep(
            &mut view,
            "saleor-api",
            graphql_key(GraphqlOp::Query, "somethingElse"),
        );
        insert_ep(
            &mut view,
            "saleor-api",
            graphql_key(GraphqlOp::Mutation, "updateProduct"),
        );

        let gid = call_id("storefront", "graphql-call:query:product");
        assert_eq!(
            candidate_triples(&view, &gid),
            vec![
                (
                    "saleor-api".to_string(),
                    "graphql:query:product".to_string(),
                    "same_key".to_string(),
                ),
                (
                    "saleor-api".to_string(),
                    "graphql:query:somethingElse".to_string(),
                    "pattern".to_string(),
                ),
            ],
            "a query consumer lists query endpoints only"
        );

        // No subscription endpoint in the index — fall back to every
        // GraphQL key, never to HTTP.
        let gid = call_id("storefront", "graphql-call:subscription:tick");
        assert_eq!(
            candidate_triples(&view, &gid),
            vec![
                (
                    "saleor-api".to_string(),
                    "graphql:query:product".to_string(),
                    "pattern".to_string(),
                ),
                (
                    "saleor-api".to_string(),
                    "graphql:query:somethingElse".to_string(),
                    "pattern".to_string(),
                ),
                (
                    "saleor-api".to_string(),
                    "graphql:mutation:updateProduct".to_string(),
                    "pattern".to_string(),
                ),
            ],
            "with no same-op endpoints every graphql endpoint is a candidate"
        );
    }

    /// HTTP consumers keep the pre-existing candidate behavior —
    /// method/template matching over HTTP endpoints only, with the
    /// `same_key` / `pattern` reason labels — so the GraphQL fix
    /// cannot drift the HTTP path. "Unchanged" means: the inclusion
    /// rule (method gate + §7.4 `match_route`, `BTreeMap` key order),
    /// a truly equal key still labeled `same_key`, and a consumer
    /// with no template still listed `pattern`. The ONLY pin this
    /// test adjusts is the gateway-bridge case appended below: its
    /// label moves from the pre-fix `same_key` (every route match
    /// collapsed to it) to `prefix_stripped`.
    #[test]
    fn http_consumer_candidates_unchanged() {
        let mut view = ContractIndex::default();
        insert_ep(
            &mut view,
            "orders",
            http_key(HttpMethod::Get, "/api/orders/{}"),
        );
        insert_ep(
            &mut view,
            "orders",
            http_key(HttpMethod::Get, "/api/users/{}"),
        );
        insert_ep(
            &mut view,
            "orders",
            http_key(HttpMethod::Post, "/api/orders/{}"),
        );
        insert_ep(
            &mut view,
            "shipping",
            http_key(HttpMethod::Get, "/api/orders/{}"),
        );
        insert_ep(
            &mut view,
            "saleor-api",
            graphql_key(GraphqlOp::Query, "product"),
        );

        // `GET /api/orders/{}` → route-matching GET endpoints, `same_key`.
        let gid = call_id("billing", "GET /api/orders/{}");
        assert_eq!(
            candidate_triples(&view, &gid),
            vec![
                (
                    "orders".to_string(),
                    "http:GET /api/orders/{}".to_string(),
                    "same_key".to_string(),
                ),
                (
                    "shipping".to_string(),
                    "http:GET /api/orders/{}".to_string(),
                    "same_key".to_string(),
                ),
            ]
        );

        // No consumer template (`GET`) → every GET endpoint, `pattern`.
        let gid = call_id("billing", "GET");
        assert_eq!(
            candidate_triples(&view, &gid),
            vec![
                (
                    "orders".to_string(),
                    "http:GET /api/orders/{}".to_string(),
                    "pattern".to_string(),
                ),
                (
                    "orders".to_string(),
                    "http:GET /api/users/{}".to_string(),
                    "pattern".to_string(),
                ),
                (
                    "shipping".to_string(),
                    "http:GET /api/orders/{}".to_string(),
                    "pattern".to_string(),
                ),
            ]
        );

        // The one adjusted pin: the gateway-bridge shape (the §15.2
        // scenario-10 pair the `list_unresolved` golden carries,
        // `GET /v1/api/orders/{}` → `GET /api/orders/{}`) used to be
        // labeled `same_key` like every other route match; it is now
        // `prefix_stripped`.
        let gid = call_id("billing", "GET /v1/api/orders/{}");
        assert_eq!(
            candidate_triples(&view, &gid),
            vec![
                (
                    "orders".to_string(),
                    "http:GET /api/orders/{}".to_string(),
                    "prefix_stripped".to_string(),
                ),
                (
                    "shipping".to_string(),
                    "http:GET /api/orders/{}".to_string(),
                    "prefix_stripped".to_string(),
                ),
            ],
            "bridge matches must be labeled prefix_stripped"
        );
    }

    /// Candidate `reason` labels reuse the §7.4 route matcher's
    /// classifier (`match_route` / `RouteMatch` — the
    /// `exact|pattern|prefix_stripped` taxonomy `route_match_label`
    /// renders for bound consumers) instead of a second matcher:
    /// `same_key` only when the consumer's key equals the endpoint
    /// key, `prefix_stripped` when the templates match only after
    /// the matcher strips the consumer's gateway prefix (the
    /// bridge shape), and `pattern` for every other route match
    /// (literal-vs-placeholder, wildcard).
    #[test]
    fn prefix_stripped_candidate_label_for_bridge_templates() {
        let mut view = ContractIndex::default();
        insert_ep(
            &mut view,
            "orders",
            http_key(HttpMethod::Get, "/api/orders/{}"),
        );

        // Truly equal key → `same_key`.
        let gid = call_id("billing", "GET /api/orders/{}");
        assert_eq!(
            candidate_triples(&view, &gid),
            vec![(
                "orders".to_string(),
                "http:GET /api/orders/{}".to_string(),
                "same_key".to_string(),
            )],
            "an identical key stays same_key"
        );

        // Wildcard/pattern match (literal id vs `{}`) → `pattern`.
        let gid = call_id("billing", "GET /api/orders/42");
        assert_eq!(
            candidate_triples(&view, &gid),
            vec![(
                "orders".to_string(),
                "http:GET /api/orders/{}".to_string(),
                "pattern".to_string(),
            )],
            "a placeholder match is not the same key — pattern"
        );

        // Gateway-bridge shape: the consumer template matches the
        // endpoint template only after `match_route` strips the
        // consumer's leading literal (`/v1`) — `RouteMatch::PrefixStripped`.
        let gid = call_id("billing", "GET /v1/api/orders/{}");
        assert_eq!(
            candidate_triples(&view, &gid),
            vec![(
                "orders".to_string(),
                "http:GET /api/orders/{}".to_string(),
                "prefix_stripped".to_string(),
            )],
            "a prefix-bridge match is prefix_stripped, not same_key"
        );

        // Boundary: `GET /v1/orders` is NOT a §7.4 match for
        // `GET /api/orders` — prefix tolerance strips only the
        // CONSUMER's leading literals, and after dropping `/v1` the
        // consumer is `/orders`, one segment short of `/api/orders`
        // (`segments_match` requires equal segment counts). The pair
        // is not a candidate at all; the bridge that does classify
        // as `PrefixStripped` is the one above (`/v1/api/orders/…`).
        let gid = call_id("billing", "GET /v1/orders");
        assert_eq!(
            candidate_triples(&view, &gid),
            Vec::new(),
            "no §7.4 match → no candidate (see the bridge pair above)"
        );
    }

    /// §9.7 gating: when the item carries a `target_service` (the
    /// HTTP `no_route_in_service` resolver named one, or GraphQL's
    /// `route_owner`), candidates come only from THAT service's
    /// endpoints — still subject to the per-kind key rules (HTTP
    /// method/template matching, GraphQL same-op preference, which
    /// now applies within the named service). Without one, the whole
    /// index stays in play. `BTreeMap` key order is unchanged.
    #[test]
    fn candidates_respect_target_service() {
        let mut view = ContractIndex::default();
        insert_ep(
            &mut view,
            "orders",
            http_key(HttpMethod::Get, "/api/orders/{}"),
        );
        insert_ep(
            &mut view,
            "shipping",
            http_key(HttpMethod::Get, "/api/orders/{}"),
        );
        insert_ep(
            &mut view,
            "saleor-api",
            graphql_key(GraphqlOp::Query, "product"),
        );
        insert_ep(
            &mut view,
            "saleor-api",
            graphql_key(GraphqlOp::Query, "somethingElse"),
        );
        insert_ep(
            &mut view,
            "saleor-catalog",
            graphql_key(GraphqlOp::Query, "product"),
        );

        // HTTP: `target_service = orders` lists only orders' endpoint,
        // even though shipping exposes the same key.
        let gid = call_id("billing", "GET /api/orders/{}");
        assert_eq!(
            candidate_triples_for(&view, &gid, Some(&ServiceName("orders".to_string()))),
            vec![(
                "orders".to_string(),
                "http:GET /api/orders/{}".to_string(),
                "same_key".to_string(),
            )],
            "a named target_service rules other services out (§9.7)"
        );
        // No target_service → both services, as before.
        assert_eq!(
            candidate_triples(&view, &gid),
            vec![
                (
                    "orders".to_string(),
                    "http:GET /api/orders/{}".to_string(),
                    "same_key".to_string(),
                ),
                (
                    "shipping".to_string(),
                    "http:GET /api/orders/{}".to_string(),
                    "same_key".to_string(),
                ),
            ],
            "without a target_service every service is listed"
        );

        // GraphQL `route_owner` gating: the same-op preference still
        // applies, within the named service — saleor-catalog's
        // `query:product` is not a candidate for a `route_owner =
        // saleor-api` item.
        let gid = call_id("storefront", "graphql-call:query:product");
        assert_eq!(
            candidate_triples_for(&view, &gid, Some(&ServiceName("saleor-api".to_string()))),
            vec![
                (
                    "saleor-api".to_string(),
                    "graphql:query:product".to_string(),
                    "same_key".to_string(),
                ),
                (
                    "saleor-api".to_string(),
                    "graphql:query:somethingElse".to_string(),
                    "pattern".to_string(),
                ),
            ],
            "route_owner gating keeps only that service's graphql endpoints"
        );
        // Without a route_owner the whole GraphQL set is listed.
        assert_eq!(
            candidate_triples(&view, &gid),
            vec![
                (
                    "saleor-api".to_string(),
                    "graphql:query:product".to_string(),
                    "same_key".to_string(),
                ),
                (
                    "saleor-api".to_string(),
                    "graphql:query:somethingElse".to_string(),
                    "pattern".to_string(),
                ),
                (
                    "saleor-catalog".to_string(),
                    "graphql:query:product".to_string(),
                    "same_key".to_string(),
                ),
            ],
            "without a route_owner every graphql endpoint is listed"
        );

        // Same-op preference is computed within the named service:
        // `saleor-catalog` has no mutation endpoint, so a mutation
        // consumer gated to it falls back to that service's GraphQL
        // endpoints (`pattern`), not to another service's mutations.
        let gid = call_id("storefront", "graphql-call:mutation:updateProduct");
        assert_eq!(
            candidate_triples_for(
                &view,
                &gid,
                Some(&ServiceName("saleor-catalog".to_string()))
            ),
            vec![(
                "saleor-catalog".to_string(),
                "graphql:query:product".to_string(),
                "pattern".to_string(),
            )],
            "same-op fallback runs inside the target_service's endpoints"
        );
    }
}
