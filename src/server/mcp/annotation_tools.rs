//! MCP tool handlers for the agent-side annotation + handoff layer
//! (M4 plan §4.3). Five tools, all `Always` classified, all
//! routed through the central dispatcher:
//!
//! - `add_annotation`             — write one annotation
//! - `list_annotations`           — read with filters + live-staleness
//! - `resolve_annotation`         — close an open annotation
//! - `leave_handoff_note`         — write a workspace-scoped note for
//!                                  the next agent that registers
//! - `get_pending_handoffs`       — list notes left by previous agents
//!
//! The five `pub fn run_*` functions take `&LainServer` and return
//! `Result<serde_json::Value, String>` — the same shape
//! `presence_tools.rs` and `federation_tools.rs` use, so the
//! dispatcher's `dispatch_presence_tool_outcome`-style wrapper can
//! route them through the standard error-envelope path.
//!
//! Live-staleness for `list_annotations` and `get_pending_handoffs`
//! requires a `exists: &dyn Fn(&AnnotationTarget) -> bool` resolver.
//! For `file` and `repo` targets we use the workspace path; for
//! `symbol` targets we defer to the active per-repo graph's
//! `nodes()` membership (the agent is responsible for naming a
//! `repo_id` when listing cross-repo symbols; the dispatcher routes
//! to the right per-repo store before reaching this layer).

use super::presence_tools::authenticate;
use crate::server::annotations::{
    AddAnnotationInputs, AnnotationKind, AnnotationStatus, AnnotationTarget, ListFilter,
};
use crate::server::presence::AgentId;
use crate::server::LainServer;
use serde_json::{json, Value};

fn str_arg(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("Missing required argument: {key}"))
}

fn opt_str_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(str::to_owned)
}

fn parse_target_spec(v: &Value) -> Result<AnnotationTarget, String> {
    let kind = v
        .get("kind")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "target.kind is required".to_string())?;
    match kind {
        "symbol" => Ok(AnnotationTarget::Symbol {
            symbol: str_arg(v, "symbol")?,
        }),
        "file" => Ok(AnnotationTarget::File {
            file: str_arg(v, "file")?,
        }),
        "repo" => Ok(AnnotationTarget::Repo {
            repo_id: str_arg(v, "repo_id")?,
        }),
        "edge" => Ok(AnnotationTarget::Edge {
            from: str_arg(v, "from")?,
            to: str_arg(v, "to")?,
        }),
        other => Err(format!("Unknown target.kind: {other}")),
    }
}

fn parse_kind(v: Option<&Value>) -> Result<AnnotationKind, String> {
    let Some(v) = v else {
        return Ok(AnnotationKind::Note);
    };
    let s = v
        .as_str()
        .ok_or_else(|| "kind must be a string".to_string())?;
    AnnotationKind::parse(s).ok_or_else(|| format!("Unknown kind: {s}"))
}

fn parse_status(v: Option<&Value>) -> Result<Option<AnnotationStatus>, String> {
    let Some(v) = v else {
        return Ok(None);
    };
    let s = v
        .as_str()
        .ok_or_else(|| "status must be a string".to_string())?;
    AnnotationStatus::parse(s)
        .map(Some)
        .ok_or_else(|| format!("Unknown status: {s}"))
}

fn parse_refs(v: Option<&Value>) -> Result<Vec<AnnotationTarget>, String> {
    let Some(v) = v else {
        return Ok(Vec::new());
    };
    let arr = v
        .as_array()
        .ok_or_else(|| "refs must be an array".to_string())?;
    arr.iter().map(parse_target_spec).collect()
}

fn parse_limit(v: Option<&Value>) -> Result<Option<u32>, String> {
    let Some(v) = v else {
        return Ok(Some(100));
    };
    let n = v
        .as_u64()
        .ok_or_else(|| "limit must be a positive integer".to_string())?;
    Ok(Some(n.min(1000) as u32))
}

/// Resolve the target to a single repo id. Cross-cutting helpers
/// (e.g. `leave_handoff_note`) call this so the federation backend
/// never has to guess where a `symbol` or `file` lives.
fn target_to_repo(
    server: &LainServer,
    target: &AnnotationTarget,
) -> Result<crate::federation::repo_id::RepoId, String> {
    use crate::federation::repo_id::RepoId;
    match target {
        AnnotationTarget::Repo { repo_id } => {
            let rid = RepoId::new(repo_id).map_err(|e| format!("Invalid repo_id: {e}"))?;
            // Reject unknown repos up front. Otherwise
            // `AnnotationStore::open` would happily create a
            // `<state_dir>/annotations/<repo>.sqlite` for a never-
            // registered id, the row would land there, and
            // `list_annotations`/`resolve_annotation` would never
            // find it because they enumerate only registered repos.
            if server
                .federation()
                .is_some_and(|f| f.get_repo(&rid).is_none())
            {
                return Err(format!("Unknown repo_id: {repo_id}"));
            }
            if server.federation().is_none() && server.federation_repos().is_empty() {
                // Single-workspace mode has no federation registry;
                // the lone workspace IS the only valid repo. We
                // accept the target verbatim so single-workspace
                // servers can target their own repo by id without
                // a federation registration round-trip.
            }
            Ok(rid)
        }
        AnnotationTarget::File { .. } | AnnotationTarget::Symbol { .. } => {
            // Single-repo mode: pin to the lone repo. Federation
            // mode: `add_annotation` is intentionally `Always`-
            // classified and the caller is expected to name the
            // repo via `AnnotationTarget::Repo { repo_id }` (or
            // via the registry's cross-repo listing).
            let repos = server.federation_repos();
            match repos.len() {
                1 => Ok(repos.into_iter().next().unwrap()),
                _ => Err(
                    "Cross-repo annotations require target = {kind: 'repo', repo_id: '...'} \
                     so the registry knows where to store the row"
                        .into(),
                ),
            }
        }
        AnnotationTarget::Edge { .. } => Err(
            "Edge targets require target = {kind: 'repo', repo_id: '...'} \
             so the registry knows where to store the row"
                .into(),
        ),
    }
}

fn author_from_args(server: &LainServer, args: &Value) -> Result<AgentId, String> {
    if let Some(s) = args.get("author").and_then(|v| v.as_str()) {
        return Ok(AgentId(s.to_string()));
    }
    let token = str_arg(args, "session_token")?;
    Ok(authenticate(server, &token)?.id)
}

pub fn run_add_annotation(server: &LainServer, args: Value) -> Result<Value, String> {
    let target_v = args
        .get("target")
        .ok_or_else(|| "Missing required argument: target".to_string())?;
    let target = parse_target_spec(target_v)?;
    let kind = parse_kind(args.get("kind"))?;
    let body = str_arg(&args, "body")?;
    let author = author_from_args(server, &args)?;
    let refs = parse_refs(args.get("refs"))?;
    let repo = target_to_repo(server, &target)?;
    let inputs = AddAnnotationInputs {
        target,
        kind,
        body,
        author,
        refs,
    };
    let ann = inputs.into_annotation();
    let store = server
        .annotations()
        .store_for(&repo)
        .map_err(|e| e.to_string())?;
    store.add(&ann).map_err(|e| e.to_string())?;
    Ok(json!({
        "id": ann.id,
        "created_at_unix_ms": ann.created_at_unix_ms,
        "repo_id": repo.as_str(),
    }))
}

pub fn run_list_annotations(server: &LainServer, args: Value) -> Result<Value, String> {
    let target = args.get("target").map(parse_target_spec).transpose()?;
    let author = opt_str_arg(&args, "author").map(AgentId);
    // `kind` is OPTIONAL — only filter by it when the caller passed
    // the argument. Pre-fix bug: parse_kind defaulted to Note when
    // missing, then `Some(kind)` made every list call notes-only.
    let kind = if args.get("kind").is_some() {
        Some(parse_kind(args.get("kind"))?)
    } else {
        None
    };
    let status = parse_status(args.get("status"))?;
    let limit = parse_limit(args.get("limit"))?;
    let filter = ListFilter {
        target,
        author,
        kind,
        status,
        limit,
    };

    let repos = server.federation_repos();
    let annotations = server
        .annotations()
        .list_all(&repos, &filter, &|t| target_exists(server, t))
        .map_err(|e| e.to_string())?;
    Ok(json!({ "annotations": annotations }))
}

pub fn run_resolve_annotation(server: &LainServer, args: Value) -> Result<Value, String> {
    let id = str_arg(&args, "id")?;
    let by = if let Some(token) = args.get("session_token").and_then(|v| v.as_str()) {
        authenticate(server, token)?.id
    } else {
        opt_str_arg(&args, "resolved_by")
            .map(AgentId)
            .ok_or_else(|| "Missing required argument: session_token or resolved_by".to_string())?
    };

    let repos = server.federation_repos();
    let mut last_err: Option<String> = None;
    for repo in &repos {
        let store = server
            .annotations()
            .store_for(repo)
            .map_err(|e| e.to_string())?;
        match store.resolve(&id, &by) {
            Ok(a) => return Ok(json!({ "resolved": a })),
            Err(crate::error::LainError::NotFound(_)) => continue,
            Err(e) => last_err = Some(e.to_string()),
        }
    }
    Err(last_err.unwrap_or_else(|| format!("annotation {id} not found in any registered repo")))
}

pub fn run_leave_handoff_note(server: &LainServer, args: Value) -> Result<Value, String> {
    let body = str_arg(&args, "body")?;
    let scope = opt_str_arg(&args, "scope");
    let refs = parse_refs(args.get("refs"))?;
    let author = author_from_args(server, &args)?;

    let repos = server.federation_repos();
    let repo = match repos.len() {
        1 => repos.into_iter().next().unwrap(),
        _ => {
            return Err(
                "leave_handoff_note currently requires a single registered repo; \
                 scope = 'repo:<id>' is reserved for the cross-repo expansion"
                    .into(),
            )
        }
    };

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let expires_at_unix_ms = now_ms + 24 * 60 * 60 * 1000;

    let target = AnnotationTarget::Repo {
        repo_id: repo.as_str().to_string(),
    };
    let mut body_text = body;
    if let Some(scope) = scope.as_deref() {
        body_text = format!("[scope:{scope}]\n{body_text}");
    }
    let inputs = AddAnnotationInputs {
        target,
        kind: AnnotationKind::Note,
        body: body_text,
        author,
        refs,
    };
    let ann = inputs.into_annotation();
    let store = server
        .annotations()
        .store_for(&repo)
        .map_err(|e| e.to_string())?;
    store.add(&ann).map_err(|e| e.to_string())?;
    Ok(json!({
        "id": ann.id,
        "expires_at_unix_ms": expires_at_unix_ms,
    }))
}

pub fn run_get_pending_handoffs(server: &LainServer, args: Value) -> Result<Value, String> {
    let since_unix_ms = args
        .get("since_unix_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let scope = opt_str_arg(&args, "scope");

    let filter = ListFilter {
        kind: Some(AnnotationKind::Note),
        status: Some(AnnotationStatus::Open),
        limit: Some(200),
        ..Default::default()
    };

    let repos = server.federation_repos();
    let annotations = server
        .annotations()
        .list_all(&repos, &filter, &|t| target_exists(server, t))
        .map_err(|e| e.to_string())?;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let ttl_ms = 24 * 60 * 60 * 1000;

    let handoffs: Vec<_> = annotations
        .into_iter()
        .filter(|a| {
            a.created_at_unix_ms >= since_unix_ms
                && now_ms.saturating_sub(a.created_at_unix_ms) <= ttl_ms
        })
        .filter(|a| match scope.as_deref() {
            None | Some("workspace") => true,
            Some(s) => a.body.contains(&format!("[scope:{s}]")),
        })
        .collect();

    Ok(json!({ "handoffs": handoffs }))
}

fn target_exists(server: &LainServer, target: &AnnotationTarget) -> bool {
    match target {
        AnnotationTarget::Repo { repo_id } => crate::federation::repo_id::RepoId::new(repo_id)
            .ok()
            .and_then(|rid| server.federation().map(|f| f.get_repo(&rid).is_some()))
            .unwrap_or(false),
        AnnotationTarget::File { .. } | AnnotationTarget::Symbol { .. } => true,
        AnnotationTarget::Edge { .. } => false,
    }
}

/// Look up annotations targeting the symbols/edges visited by a
/// graph query, for auto-include in `explain_symbol` /
/// `get_blast_radius` markdown. Returns summaries only.
pub fn summaries_for_targets(
    server: &LainServer,
    repo: &crate::federation::repo_id::RepoId,
    targets: &[AnnotationTarget],
) -> Vec<crate::server::annotations::AnnotationSummary> {
    use crate::server::annotations::{AnnotationSummary, ListQuery};
    let Ok(store) = server.annotations().store_for(repo) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for target in targets {
        let filter = ListFilter {
            target: Some(target.clone()),
            status: Some(AnnotationStatus::Open),
            limit: Some(8),
            ..Default::default()
        };
        let q = ListQuery {
            filter: &filter,
            exists: &|_| true,
        };
        let Ok(rows) = store.list_with_staleness(&q) else {
            continue;
        };
        for a in rows {
            out.push(AnnotationSummary::from_full(&a));
        }
    }
    out
}
