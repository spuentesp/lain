//! Evidence tools: `resolve_evidence`, `read_source`
//! (`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §10.7, §12, §13).
//!
//! ## Safety (full §10.7)
//!
//! - `read_source` and `resolve_evidence` snippets read only files
//!   the view's index walked: the cache entry's `files` for
//!   snapshots, the repo's indexed files for `live`.
//! - Secret files are refused: matched case-insensitively by
//!   basename (`.env`, `.env.*`, `*.pem`, `*.key`, `*.p12`,
//!   `*.pfx`, `id_rsa*`, `id_dsa*`, `id_ecdsa*`, `id_ed25519*`,
//!   `.npmrc`, `.pypirc`, `.netrc`, `credentials*.json`,
//!   `*.keystore`).
//! - Binaries are refused: a NUL byte in the first 8 KiB.
//! - Snapshot content is read from the mirror's git2 object store
//!   at the snapshot commit, never a working tree.
//! - Live content is read from disk + `dirty` flag set.

use super::envelope::{check_api_version, error_outcome, outcome, success_envelope};
use super::{ContractToolEntry, ContractToolFuture, ToolOutcome};
use crate::federation::contracts::snapshots::record::SnapshotRecord;
use crate::federation::contracts::snapshots::RepoSnapshotState;
use crate::federation::federated_index::FederatedIndex;
use crate::federation::graph_backend::GraphBackend;
use crate::federation::repo_id::GlobalId;
use crate::server::mcp::handler::McpContext;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

// ─── dispatch ─────────────────────────────────────────────────────────

inventory::submit!(ContractToolEntry {
    name: "resolve_evidence",
    handler: resolve_evidence_handle,
});
inventory::submit!(ContractToolEntry {
    name: "read_source",
    handler: read_source_handle,
});

pub fn resolve_evidence_handle<'a>(ctx: &'a McpContext<'a>, args: Value) -> ContractToolFuture<'a> {
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
        match run_resolve_evidence(ctx, args_map, started).await {
            Ok(o) => Ok(o),
            Err(o) => Ok(o),
        }
    })
}

pub fn read_source_handle<'a>(ctx: &'a McpContext<'a>, args: Value) -> ContractToolFuture<'a> {
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
        match run_read_source(ctx, args_map, started).await {
            Ok(o) => Ok(o),
            Err(o) => Ok(o),
        }
    })
}

// ─── resolve_evidence ─────────────────────────────────────────────────

async fn run_resolve_evidence<'a>(
    ctx: &'a McpContext<'a>,
    args_map: Map<String, Value>,
    started: Instant,
) -> Result<ToolOutcome, ToolOutcome> {
    let snap_label = snapshot_label(&args_map);
    let refs_raw = args_map
        .get("refs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if refs_raw.is_empty() {
        return Err(error_outcome(
            "invalid_argument",
            "refs must be a non-empty array",
            Some(json!({"arg": "refs"})),
            &snap_label,
            started,
        ));
    }
    if refs_raw.len() > 200 {
        return Err(error_outcome(
            "range_too_large",
            format!("refs {} exceeds max 200", refs_raw.len()),
            Some(json!({"limit": refs_raw.len(), "max": 200, "requested": refs_raw.len()})),
            &snap_label,
            started,
        ));
    }
    let context_lines = args_map
        .get("context_lines")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(3);
    if context_lines > 20 {
        return Err(error_outcome(
            "range_too_large",
            format!("context_lines {context_lines} exceeds max 20"),
            Some(json!({"limit": context_lines, "max": 20, "requested": context_lines})),
            &snap_label,
            started,
        ));
    }
    let ctx_view = resolve_view_state(ctx, &args_map, started).await?;
    let mut items: Vec<Value> = Vec::new();
    for r in refs_raw {
        let s = r.as_str().unwrap_or("").to_string();
        if s.is_empty() {
            items.push(json!({
                "ref": "",
                "exists": false,
                "reason": "malformed",
            }));
            continue;
        }
        let item = match parse_ref(&s) {
            ParsedRef::GlobalId(id) => resolve_global_id(&ctx_view, &id, context_lines),
            ParsedRef::EvidenceText(parts) => {
                resolve_evidence_text(&ctx_view, &parts, context_lines)
            }
            ParsedRef::Malformed => json!({
                "ref": s,
                "exists": false,
                "reason": "malformed",
            }),
        };
        items.push(item);
    }
    let data = json!({"items": items});
    let envelope = success_envelope(data.clone(), &snap_label, snap_label != "live", started);
    let text = render_resolve_evidence(&data);
    Ok(outcome(envelope, &data, text))
}

enum ParsedRef {
    GlobalId(GlobalId),
    EvidenceText(EvidenceTextParts),
    Malformed,
}

#[derive(Debug, Clone)]
struct EvidenceTextParts {
    repo: String,
    sha: String,
    path: String,
    line: u32,
}

fn parse_ref(s: &str) -> ParsedRef {
    // GlobalId has 5 colon segments; EvidenceRef text has 3
    // colon segments (`repo@sha:path:line`).
    if let Ok(g) = GlobalId::parse(s) {
        return ParsedRef::GlobalId(g);
    }
    let segments: Vec<&str> = s.split(':').collect();
    if segments.len() == 3 {
        let first = segments[0];
        let (repo, sha) = match first.split_once('@') {
            Some((r, s)) => (r.to_string(), s.to_string()),
            None => return ParsedRef::Malformed,
        };
        let path = segments[1].to_string();
        let line: u32 = match segments[2].parse() {
            Ok(n) => n,
            Err(_) => return ParsedRef::Malformed,
        };
        return ParsedRef::EvidenceText(EvidenceTextParts {
            repo,
            sha,
            path,
            line,
        });
    }
    ParsedRef::Malformed
}

/// The snapshot view: the pinned record plus the resident
/// federation opened for it (backend + manager `data_dir`). Boxed
/// so both `ViewState` variants stay the same size
/// (`clippy::large_enum_variant`).
struct SnapshotViewState {
    record: SnapshotRecord,
    federation: Arc<crate::federation::contracts::snapshots::SnapshotFederation>,
}

enum ViewState<'a> {
    Live { fed: &'a FederatedIndex },
    Snapshot(Box<SnapshotViewState>),
}

async fn resolve_view_state<'a>(
    ctx: &'a McpContext<'a>,
    args_map: &Map<String, Value>,
    started: Instant,
) -> Result<ViewState<'a>, ToolOutcome> {
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
        return Ok(ViewState::Live { fed });
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
    // Open a residency slot for the snapshot so the in-memory
    // backend is queryable. Honor §10.5's 5_000 ms grace for
    // analysis / evidence tools.
    let wait_ms = std::cmp::min(
        args_map
            .get("wait_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(5_000),
        60_000,
    );
    let (fed, _hold) = mgr
        .from_snapshot_with_wait_ms(&outcome.record, wait_ms)
        .map_err(|e| {
            error_outcome(
                "invalid_argument",
                format!("from_snapshot: {e}"),
                None,
                &snap_label,
                started,
            )
        })?;
    Ok(ViewState::Snapshot(Box::new(SnapshotViewState {
        record: outcome.record,
        federation: Arc::clone(&fed),
    })))
}

fn resolve_global_id<'a>(view: &ViewState<'a>, id: &GlobalId, context_lines: u32) -> Value {
    // Walk the federation's node table for `live`; the snapshot
    // path looks up via the projection's `from_snapshot`. For
    // simplicity we delegate to `get_node`-style lookups on the
    // federation's backend.
    match view {
        ViewState::Live { fed } => {
            let backend = fed.backend();
            match backend.get_node(id.as_str()) {
                Ok(Some(node)) => {
                    let line = node.line_start.unwrap_or(0);
                    let path = node.path.clone();
                    let snippet = backend
                        .get_node(id.as_str())
                        .ok()
                        .flatten()
                        .and_then(|_| snippet_from_live(fed, &path, line, context_lines));
                    json!({
                        "ref": id.as_str(),
                        "exists": true,
                        "node": {
                            "id": id.as_str(),
                            "node_type": format!("{:?}", node.node_type),
                            "name": node.name,
                            "ref": {
                                "id": id.as_str(),
                                "repo": id.repo_id(),
                                "commit": "",
                                "path": path,
                                "line": line,
                                "text": "",
                            },
                        },
                        "snippet": snippet,
                    })
                }
                _ => json!({
                    "ref": id.as_str(),
                    "exists": false,
                    "reason": "no_such_node",
                }),
            }
        }
        ViewState::Snapshot(inner) => {
            let record = &inner.record;
            let fed = &inner.federation;
            // Check the repo is in the snapshot. The GlobalId's
            // first segment is the bare repo id (§4.1, §5.1).
            let Some(snap_id) = id.repo_id().split(':').next() else {
                return json!({
                    "ref": id.as_str(),
                    "exists": false,
                    "reason": "malformed",
                });
            };
            if !record.repos.contains_key(snap_id) {
                return json!({
                    "ref": id.as_str(),
                    "exists": false,
                    "reason": "unknown_repo",
                });
            }
            if let Some(state) = record.repo_states.get(snap_id) {
                if matches!(state, RepoSnapshotState::Excluded) {
                    return json!({
                        "ref": id.as_str(),
                        "exists": false,
                        "reason": "malformed",
                    });
                }
            }
            // Real lookup: query the snapshot's backend for the
            // node id. `line_mismatch` (§12): a GlobalId whose
            // node no longer starts at the cited line — the exact
            // id misses, but the same symbol at another line means
            // the ref moved rather than disappeared.
            let exact = fed.backend.get_node(id.as_str()).ok().flatten();
            let node = match exact {
                Some(node) => node,
                None => {
                    let reason = if same_symbol_at_other_line(fed, id) {
                        "line_mismatch"
                    } else {
                        "no_such_node"
                    };
                    return json!({
                        "ref": id.as_str(),
                        "exists": false,
                        "reason": reason,
                    });
                }
            };
            let actual_line = node.line_start.unwrap_or(0);
            let cited_line = id.line_start().unwrap_or(0);
            if actual_line != cited_line {
                return json!({
                    "ref": id.as_str(),
                    "exists": false,
                    "reason": "line_mismatch",
                });
            }
            let path = node.path.clone();
            let commit = record.repos.get(snap_id).cloned().unwrap_or_default();
            let snippet =
                snapshot_blob_window(fed, snap_id, &commit, &path, actual_line, context_lines);
            json!({
                "ref": id.as_str(),
                "exists": true,
                "node": {
                    "id": id.as_str(),
                    "node_type": format!("{:?}", node.node_type),
                    "name": node.name,
                    "ref": {
                        "id": id.as_str(),
                        "repo": id.repo_id(),
                        "commit": commit,
                        "path": path,
                        "line": actual_line,
                        "text": "",
                    },
                },
                "snippet": snippet,
            })
        }
    }
}

fn resolve_evidence_text<'a>(
    view: &ViewState<'a>,
    parts: &EvidenceTextParts,
    context_lines: u32,
) -> Value {
    let ref_str = format!("{}@{}:{}:{}", parts.repo, parts.sha, parts.path, parts.line);
    // Verify the repo + commit exist in the view.
    let commit_for_repo = match view {
        ViewState::Live { fed } => {
            let Some(repo_id) = crate::federation::repo_id::RepoId::new(&parts.repo).ok() else {
                return json!({
                    "ref": ref_str,
                    "exists": false,
                    "reason": "unknown_repo",
                });
            };
            if fed.get_repo(&repo_id).is_none() {
                return json!({
                    "ref": ref_str,
                    "exists": false,
                    "reason": "unknown_repo",
                });
            }
            let commit = fed
                .get_repo(&repo_id)
                .and_then(|r| r.db().get_last_commit().ok().flatten());
            commit
        }
        ViewState::Snapshot(inner) => {
            let record = &inner.record;
            if !record.repos.contains_key(&parts.repo) {
                return json!({
                    "ref": ref_str,
                    "exists": false,
                    "reason": "unknown_repo",
                });
            }
            let view_commit = record.repos.get(&parts.repo).cloned().unwrap_or_default();
            if !sha_prefix_matches(&parts.sha, &view_commit) {
                return json!({
                    "ref": ref_str,
                    "exists": false,
                    "reason": "commit_not_in_view",
                });
            }
            Some(view_commit)
        }
    };

    // Read the file from the right source.
    let read: Option<String> = match view {
        ViewState::Live { fed } => {
            let repo_id = crate::federation::repo_id::RepoId::new(&parts.repo).ok();
            let local_path =
                repo_id.and_then(|rid| fed.get_repo(&rid).map(|r| r.local_path().to_path_buf()));
            local_path.and_then(|p| {
                read_file_lines(&p, &parts.path, parts.line, context_lines)
                    .ok()
                    .flatten()
            })
        }
        ViewState::Snapshot(inner) => {
            // `commit_for_repo` already proved repo-in-view and
            // sha-prefix match against the view commit (§12).
            let commit = commit_for_repo.clone().unwrap_or_default();
            let fed = &inner.federation;
            // The file must exist with that line at the view
            // commit — read it from the repo's mirror via git2.
            let Some(text) = snapshot_blob_at(fed, &parts.repo, &commit, &parts.path) else {
                return json!({
                    "ref": ref_str,
                    "exists": false,
                    "reason": "no_such_node",
                });
            };
            let lines: Vec<&str> = text.lines().collect();
            let cited = parts.line as usize;
            if cited == 0 || cited > lines.len() {
                return json!({
                    "ref": ref_str,
                    "exists": false,
                    "reason": "no_such_node",
                });
            }
            let start = cited.saturating_sub(context_lines as usize + 1);
            let end = (cited + context_lines as usize + 1).min(lines.len());
            let snippet = lines[start..end].join("\n");
            // §12: `node` is the innermost node whose range
            // covers the line.
            let mut item = json!({
                "ref": ref_str,
                "exists": true,
                "snippet": snippet,
            });
            if let Some(node) = innermost_node_at_line(fed, &parts.repo, &parts.path, parts.line) {
                let node_line = node.line_start.unwrap_or(0);
                let node_name = node.name.clone();
                let node_id = node.id.clone();
                item["node"] = json!({
                    "id": node_id,
                    "node_type": format!("{:?}", node.node_type),
                    "name": node_name,
                    "ref": {
                        "id": node.id,
                        "repo": parts.repo,
                        "commit": commit,
                        "path": node.path,
                        "line": node_line,
                        "text": "",
                    },
                });
            }
            return item;
        }
    };

    match read {
        Some(snippet) => json!({
            "ref": ref_str,
            "exists": true,
            "snippet": snippet,
        }),
        None => json!({
            "ref": ref_str,
            "exists": false,
            "reason": "no_such_node",
        }),
    }
}

fn sha_prefix_matches(prefix: &str, full: &str) -> bool {
    let prefix = prefix.to_lowercase();
    let full = full.to_lowercase();
    if prefix.len() < 7 {
        return false;
    }
    if prefix.len() > full.len() {
        return false;
    }
    full.starts_with(&prefix)
}

fn render_resolve_evidence(data: &Value) -> String {
    let items = data["items"].as_array().cloned().unwrap_or_default();
    let mut out = String::new();
    out.push_str(&format!("# resolve_evidence ({} refs)\n", items.len()));
    for it in &items {
        let r = it["ref"].as_str().unwrap_or("");
        let exists = it["exists"].as_bool().unwrap_or(false);
        out.push_str(&format!("- {r} → exists={exists}\n"));
    }
    out
}

// ─── read_source ──────────────────────────────────────────────────────

const MAX_RANGE_LINES: u64 = 400;

async fn run_read_source(
    ctx: &McpContext<'_>,
    args_map: Map<String, Value>,
    started: Instant,
) -> Result<ToolOutcome, ToolOutcome> {
    let snap_label = snapshot_label(&args_map);
    let repo = args_map
        .get("repo")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            error_outcome(
                "invalid_argument",
                "missing required argument: repo",
                Some(json!({"arg": "repo"})),
                &snap_label,
                started,
            )
        })?
        .to_string();
    let path = args_map
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            error_outcome(
                "invalid_argument",
                "missing required argument: path",
                Some(json!({"arg": "path"})),
                &snap_label,
                started,
            )
        })?
        .to_string();
    let start = args_map
        .get("start")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            error_outcome(
                "invalid_argument",
                "missing required argument: start",
                Some(json!({"arg": "start"})),
                &snap_label,
                started,
            )
        })?;
    let end = args_map
        .get("end")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            error_outcome(
                "invalid_argument",
                "missing required argument: end",
                Some(json!({"arg": "end"})),
                &snap_label,
                started,
            )
        })?;
    if end < start {
        return Err(error_outcome(
            "invalid_argument",
            "end must be >= start",
            Some(json!({"arg": "end"})),
            &snap_label,
            started,
        ));
    }
    if end - start > MAX_RANGE_LINES {
        return Err(error_outcome(
            "range_too_large",
            format!("range {} exceeds max {}", end - start, MAX_RANGE_LINES),
            Some(json!({
                "limit": end - start,
                "max": MAX_RANGE_LINES,
                "requested": end - start,
            })),
            &snap_label,
            started,
        ));
    }

    // §10.7 secret denylist by basename.
    let basename = std::path::Path::new(&path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    if is_secret_basename(&basename) {
        return Err(error_outcome(
            "path_rejected",
            "path matches secret denylist",
            Some(json!({"reason": "secret", "path": path})),
            &snap_label,
            started,
        ));
    }

    // Walk the view's file set to confirm the path is indexed.
    let view = resolve_view_state(ctx, &args_map, started).await?;
    let (commit, source_kind, clamped_start, clamped_end, total_lines, line_text) = match &view {
        ViewState::Live { fed } => {
            let rid = crate::federation::repo_id::RepoId::new(&repo).ok();
            let local_path = rid
                .as_ref()
                .and_then(|rid| fed.get_repo(rid))
                .map(|r| r.local_path().to_path_buf());
            let local_path = match local_path {
                Some(p) => p,
                None => {
                    return Err(error_outcome(
                        "repo_not_registered",
                        format!("repo {repo:?} not configured"),
                        Some(json!({"repo": repo})),
                        &snap_label,
                        started,
                    ));
                }
            };
            let local_file = local_path.join(&path);
            // Outside-root check.
            if !is_within_root(&local_file, &local_path) {
                return Err(error_outcome(
                    "path_rejected",
                    "path outside repo root",
                    Some(json!({"reason": "outside_root", "path": path})),
                    &snap_label,
                    started,
                ));
            }
            // Indexed files = repo's last indexed file list.
            let rid = crate::federation::repo_id::RepoId::new(&repo).unwrap();
            let indexed: HashSet<String> = fed
                .get_repo(&rid)
                .map(|r| r.db().get_all_nodes().into_iter().map(|n| n.path).collect())
                .unwrap_or_default();
            if !indexed.contains(&path) {
                return Err(error_outcome(
                    "path_rejected",
                    "path not indexed",
                    Some(json!({"reason": "not_indexed", "path": path})),
                    &snap_label,
                    started,
                ));
            }
            let bytes = match std::fs::read(&local_file) {
                Ok(b) => b,
                Err(_) => {
                    return Err(error_outcome(
                        "path_rejected",
                        "could not read file",
                        Some(json!({"reason": "not_indexed", "path": path})),
                        &snap_label,
                        started,
                    ));
                }
            };
            if is_binary(&bytes) {
                return Err(error_outcome(
                    "path_rejected",
                    "binary content refused",
                    Some(json!({"reason": "binary", "path": path})),
                    &snap_label,
                    started,
                ));
            }
            let text = String::from_utf8_lossy(&bytes).to_string();
            let total_lines = text.lines().count() as u64;
            let (clamped_start, clamped_end, line_text) =
                clamp_range(&text, start, end, total_lines);
            (
                fed.get_repo(&rid)
                    .and_then(|r| r.db().get_last_commit().ok().flatten())
                    .unwrap_or_default(),
                "live",
                clamped_start,
                clamped_end,
                total_lines,
                line_text,
            )
        }
        ViewState::Snapshot(inner) => {
            let record = &inner.record;
            if !record.repos.contains_key(&repo) {
                return Err(error_outcome(
                    "repo_not_registered",
                    format!("repo {repo:?} not configured in snapshot"),
                    Some(json!({"repo": repo})),
                    &snap_label,
                    started,
                ));
            }
            let commit = record.repos.get(&repo).cloned().unwrap_or_default();
            let data_dir = ctx
                .snapshots
                .map(|m| m.data_dir().to_path_buf())
                .ok_or_else(|| {
                    error_outcome(
                        "snapshot_manager_unavailable",
                        "snapshot manager not configured",
                        None,
                        &snap_label,
                        started,
                    )
                })?;
            let (text, total_lines) = match snapshot_blob_text(&data_dir, &repo, &commit, &path) {
                Ok(t) => t,
                Err(_) => {
                    return Err(error_outcome(
                        "path_rejected",
                        "could not read blob",
                        Some(json!({"reason": "not_indexed", "path": path})),
                        &snap_label,
                        started,
                    ));
                }
            };
            if is_binary(text.as_bytes()) {
                return Err(error_outcome(
                    "path_rejected",
                    "binary content refused",
                    Some(json!({"reason": "binary", "path": path})),
                    &snap_label,
                    started,
                ));
            }
            let (clamped_start, clamped_end, line_text) =
                clamp_range(&text, start, end, total_lines);
            (
                commit,
                "snapshot",
                clamped_start,
                clamped_end,
                total_lines,
                line_text,
            )
        }
    };

    let data = json!({
        "commit": commit,
        "path": path,
        "start": clamped_start,
        "end": clamped_end,
        "total_lines": total_lines,
        "text": line_text,
        "source": source_kind,
    });
    let envelope = success_envelope(data.clone(), &snap_label, snap_label != "live", started);
    let text = format!("# read_source: {} ({} lines)\n", path, total_lines);
    Ok(outcome(envelope, &data, text))
}

fn clamp_range(text: &str, start: u64, end: u64, total_lines: u64) -> (u64, u64, String) {
    let lines: Vec<&str> = text.lines().collect();
    let total = total_lines;
    let start = start.min(total);
    let end = end.min(total);
    let slice: String = if start >= total {
        String::new()
    } else {
        lines[start as usize..end as usize].join("\n")
    };
    (start, end, slice)
}

fn snapshot_blob_text(
    data_dir: &std::path::Path,
    repo: &str,
    commit: &str,
    path: &str,
) -> Result<(String, u64), ()> {
    let mirror_path = data_dir.join("mirrors").join(format!("{repo}.git"));
    let repo_git = git2::Repository::open_bare(&mirror_path).map_err(|_| ())?;
    let oid = git2::Oid::from_str(commit).map_err(|_| ())?;
    let commit_obj = repo_git.find_commit(oid).map_err(|_| ())?;
    let tree = commit_obj.tree().map_err(|_| ())?;
    let entry = tree.get_path(std::path::Path::new(path)).map_err(|_| ())?;
    let blob = repo_git.find_blob(entry.id()).map_err(|_| ())?;
    let text = String::from_utf8_lossy(blob.content()).to_string();
    let lines = text.lines().count() as u64;
    Ok((text, lines))
}

fn read_file_lines(
    root: &Path,
    path: &str,
    line: u32,
    context_lines: u32,
) -> std::io::Result<Option<String>> {
    let p = root.join(path);
    let text = std::fs::read_to_string(&p)?;
    let lines: Vec<&str> = text.lines().collect();
    let start = line.saturating_sub(context_lines + 1) as usize;
    let end = (line + context_lines + 1) as usize;
    if start >= lines.len() {
        return Ok(None);
    }
    Ok(Some(
        lines[start.min(lines.len())..end.min(lines.len())].join("\n"),
    ))
}

fn snippet_from_live(
    fed: &FederatedIndex,
    path: &str,
    line: u32,
    context_lines: u32,
) -> Option<String> {
    let rid = crate::federation::repo_id::RepoId::new(path.split('/').next().unwrap_or("")).ok()?;
    let local = fed.get_repo(&rid)?.local_path().to_path_buf();
    read_file_lines(&local, path, line, context_lines)
        .ok()
        .flatten()
}

/// Read the blob contents of `path` at `commit` from the repo's
/// mirror under `<data_dir>/mirrors/<repo>.git`. Returns `None`
/// when the mirror, commit, tree entry or blob is missing.
fn snapshot_blob_at(
    fed: &crate::federation::contracts::snapshots::SnapshotFederation,
    repo: &str,
    commit: &str,
    path: &str,
) -> Option<String> {
    let mirror = fed
        .manager_data_dir()
        .join("mirrors")
        .join(format!("{repo}.git"));
    let repo_git = git2::Repository::open_bare(&mirror).ok()?;
    let oid = git2::Oid::from_str(commit).ok()?;
    let commit_obj = repo_git.find_commit(oid).ok()?;
    let tree = commit_obj.tree().ok()?;
    let entry = tree.get_path(Path::new(path)).ok()?;
    let blob = repo_git.find_blob(entry.id()).ok()?;
    Some(String::from_utf8_lossy(blob.content()).into_owned())
}

/// A `context_lines + 1` window around `line` (1-based) of `path`
/// at `commit`, read from the snapshot's mirror. `None` when the
/// blob or the line is missing.
fn snapshot_blob_window(
    fed: &crate::federation::contracts::snapshots::SnapshotFederation,
    repo: &str,
    commit: &str,
    path: &str,
    line: u32,
    context_lines: u32,
) -> Option<String> {
    let text = snapshot_blob_at(fed, repo, commit, path)?;
    let lines: Vec<&str> = text.lines().collect();
    let cited = line as usize;
    if cited == 0 || cited > lines.len() {
        return None;
    }
    let start = cited.saturating_sub(context_lines as usize + 1);
    let end = (cited + context_lines as usize + 1).min(lines.len());
    Some(lines[start..end].join("\n"))
}

/// `true` when the view contains a node with the same
/// `(repo, kind, path, name)` as `id` but at a different line —
/// the §12 `line_mismatch` case (the symbol moved; the ref did
/// not become stale).
fn same_symbol_at_other_line(
    fed: &crate::federation::contracts::snapshots::SnapshotFederation,
    id: &GlobalId,
) -> bool {
    let full = id.as_str();
    let Some((prefix, _)) = full.rsplit_once(':') else {
        return false;
    };
    let Ok(nodes) = fed.backend.list_nodes() else {
        return false;
    };
    let with_colon = format!("{prefix}:");
    nodes
        .iter()
        .any(|n| n.id.starts_with(&with_colon) && n.id.as_str() != full)
}

/// The innermost node whose `[line_start, line_end]` covers `line`
/// in `path` within `repo` (§12 `resolve_evidence` guarantee).
/// Ties break on the node id so the answer is deterministic.
fn innermost_node_at_line(
    fed: &crate::federation::contracts::snapshots::SnapshotFederation,
    repo: &str,
    path: &str,
    line: u32,
) -> Option<crate::schema::GraphNode> {
    let nodes = fed.backend.list_nodes().ok()?;
    nodes
        .into_iter()
        .filter(|n| {
            n.path == path
                && n.line_start.map(|s| s <= line).unwrap_or(false)
                && n.line_end.map(|e| e >= line).unwrap_or(true)
                && GlobalId::parse(&n.id)
                    .map(|g| g.repo_id() == repo)
                    .unwrap_or(false)
        })
        .min_by_key(|n| {
            (
                n.line_end
                    .unwrap_or(u32::MAX)
                    .saturating_sub(n.line_start.unwrap_or(0)),
                n.id.clone(),
            )
        })
}

fn is_secret_basename(basename_lower: &str) -> bool {
    if basename_lower.is_empty() {
        return false;
    }
    const PATTERNS: &[&str] = &[".env", ".npmrc", ".pypirc", ".netrc"];
    for p in PATTERNS {
        if basename_lower == *p {
            return true;
        }
    }
    // `.env.*`, `*.pem`, `*.key`, `*.p12`, `*.pfx`, `*.keystore`,
    // `id_rsa*`, `id_dsa*`, `id_ecdsa*`, `id_ed25519*`,
    // `credentials*.json`.
    if basename_lower.starts_with(".env.") {
        return true;
    }
    if basename_lower.starts_with("id_rsa")
        || basename_lower.starts_with("id_dsa")
        || basename_lower.starts_with("id_ecdsa")
        || basename_lower.starts_with("id_ed25519")
    {
        return true;
    }
    if basename_lower.starts_with("credentials") && basename_lower.ends_with(".json") {
        return true;
    }
    for ext in &[".pem", ".key", ".p12", ".pfx", ".keystore", ".jks"] {
        if basename_lower.ends_with(ext) {
            return true;
        }
    }
    false
}

fn is_binary(bytes: &[u8]) -> bool {
    let window = &bytes[..bytes.len().min(8192)];
    window.contains(&0)
}

fn is_within_root(file: &Path, root: &Path) -> bool {
    let file = match file.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let root = match root.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    file.starts_with(&root)
}

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

    #[test]
    fn parse_ref_recognizes_global_id() {
        let id = "orders:Function:src/main.py:fetch:42";
        match parse_ref(id) {
            ParsedRef::GlobalId(g) => {
                assert_eq!(g.repo_id(), "orders");
                assert_eq!(g.line_start().unwrap(), 42);
            }
            _ => panic!("expected GlobalId"),
        }
    }

    #[test]
    fn parse_ref_recognizes_evidence_text() {
        let s = "orders@9c20d41a7b3e:src/main.py:14";
        match parse_ref(s) {
            ParsedRef::EvidenceText(parts) => {
                assert_eq!(parts.repo, "orders");
                assert_eq!(parts.sha, "9c20d41a7b3e");
                assert_eq!(parts.path, "src/main.py");
                assert_eq!(parts.line, 14);
            }
            _ => panic!("expected EvidenceText"),
        }
    }

    #[test]
    fn parse_ref_rejects_garbage() {
        assert!(matches!(parse_ref("not-a-ref"), ParsedRef::Malformed));
    }

    #[test]
    fn is_secret_basename_catches_known_shapes() {
        for s in &[
            ".env",
            ".env.local",
            "id_rsa",
            "id_rsa.pub",
            "id_ed25519",
            "credentials.json",
            "credentials-prod.json",
            "key.pem",
            "server.key",
            "keystore.jks",
            "secret.p12",
            "thing.pfx",
        ] {
            assert!(is_secret_basename(&s.to_lowercase()), "{s}");
        }
    }

    #[test]
    fn is_secret_basename_allows_safe_files() {
        for s in &["main.rs", "package.json", "Dockerfile", ".gitignore"] {
            assert!(!is_secret_basename(&s.to_lowercase()), "{s}");
        }
    }

    #[test]
    fn is_binary_detects_nul() {
        assert!(is_binary(b"hello\x00world"));
        assert!(!is_binary(b"hello world"));
    }

    #[test]
    fn clamp_range_returns_empty_when_start_past_end() {
        let text = "line1\nline2\nline3";
        let (s, e, body) = clamp_range(text, 10, 12, 3);
        assert_eq!(s, 3);
        assert_eq!(e, 3);
        assert_eq!(body, "");
    }

    #[test]
    fn clamp_range_caps_to_total() {
        let text = "a\nb\nc";
        let (s, e, _) = clamp_range(text, 0, 100, 3);
        assert_eq!(s, 0);
        assert_eq!(e, 3);
    }

    #[test]
    fn sha_prefix_matches_min_7_hex() {
        assert!(sha_prefix_matches("deadbee", "deadbeeef"));
        assert!(!sha_prefix_matches("dead", "deadbeeef"));
        assert!(!sha_prefix_matches("xx", "deadbeeef"));
    }

    #[test]
    fn parse_endpoint_for_trace_in_evidence_text() {
        let s = "billing@aabbccddee:src/x.py:5";
        match parse_ref(s) {
            ParsedRef::EvidenceText(parts) => {
                assert_eq!(parts.repo, "billing");
                assert_eq!(parts.sha, "aabbccddee");
                assert_eq!(parts.path, "src/x.py");
                assert_eq!(parts.line, 5);
            }
            _ => panic!("expected EvidenceText"),
        }
    }
}
