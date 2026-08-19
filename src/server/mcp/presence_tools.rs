//! MCP tools for the multiplayer layer.
//!
//! Most tools read or mutate `PresenceRegistry` + `OccupancyMap` on the
//! `LainServer`. Tools that mutate state return the new state; tools
//! that read return JSON the agent can render. `detect_overlap` is the
//! one exception: it consults git and the federation's repo roots rather
//! than live presence state, because it answers a commit-time question
//! ("did two refs touch the same symbols?") rather than a live one.

use crate::server::ingest::LainServer;
use crate::server::presence::{
    AgentId, AgentKind, AgentMode, ClaimIntent, ClaimRequest, OccupancyEntry,
    PresenceEvent,
};
use crate::server::schema::NodeType;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
pub struct RegisterAgentArgs {
    pub name: String,
    pub kind: Option<String>,
    pub mode: Option<String>,
    pub parent_session_id: Option<String>,
    pub pid: Option<u32>,
}

pub fn run_register_agent(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: RegisterAgentArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let kind = a.kind.as_deref().map(AgentKind::parse).unwrap_or(AgentKind::Other("unknown".into()));
    let mode = a.mode.as_deref().map(AgentMode::parse).unwrap_or(AgentMode::Interactive);
    let parent = a.parent_session_id.map(AgentId);
    let session = server.presence.register(a.name, kind, mode, a.pid, parent);
    let _ = server.presence_event_tx.send(PresenceEvent::AgentJoined(session.clone()));
    let expires_at_unix = system_time_to_unix_secs(session.started_at)
        + system_time_to_unix_secs_delta(server.presence_expires_after());
    Ok(json!({
        "agent_id": session.id.as_str(),
        "session_token": session.session_token,
        "expires_at_unix": expires_at_unix,
    }))
}

#[derive(Debug, Deserialize)]
pub struct HeartbeatArgs {
    pub agent_id: String,
    pub session_token: String,
}

pub fn run_heartbeat(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: HeartbeatArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let agent_id = AgentId(a.agent_id);
    server.presence.heartbeat(&agent_id, &a.session_token)
        .map_err(|e| e.to_string())?;
    // Wishlist #5 fix: refresh the staleness clock on every claim the
    // agent holds so `last_seen_unix` actually advances with heartbeats
    // instead of being frozen at `claimed_at`. The presence and
    // occupancy registries have separate locks, so this is two
    // separate calls; the staleness clock now advances as long as both
    // succeed (the presence call above already errored on auth).
    server.occupancy.touch(&agent_id);
    Ok(json!({ "ok": true }))
}

#[derive(Debug, Deserialize)]
pub struct ListActiveAgentsArgs {
    pub include_background: Option<bool>,
}

pub fn run_list_active_agents(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: ListActiveAgentsArgs = serde_json::from_value(args).unwrap_or(ListActiveAgentsArgs { include_background: None });
    let sessions = server.presence.list_active(a.include_background.unwrap_or(false));
    let out: Vec<Value> = sessions.into_iter().map(|s| {
        let claims = server.occupancy.list_for_agent(&s.id);
        json!({
            "agent_id": s.id.as_str(),
            "name": s.name,
            "kind": s.kind.as_str(),
            "mode": s.mode.as_str(),
            "started_at": system_time_to_unix_secs(s.started_at),
            "last_heartbeat": system_time_to_unix_secs(s.last_heartbeat),
            "claims_count": claims.len(),
        })
    }).collect();
    Ok(json!(out))
}

#[derive(Debug, Deserialize)]
pub struct WhoAmIArgs {
    pub session_token: String,
}

pub fn run_who_am_i(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: WhoAmIArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let session = server.presence.by_token(&a.session_token).ok_or("unknown session token")?;
    let claims = server.occupancy.list_for_agent(&session.id);
    let parent_session_id = session.parent_session_id.as_ref().map(|p| p.as_str().to_string());
    Ok(json!({
        "agent_id": session.id.as_str(),
        "name": session.name,
        "kind": session.kind.as_str(),
        "mode": session.mode.as_str(),
        "parent_session_id": parent_session_id,
        "claims": claims.into_iter().map(|c| json!({
            "path": c.path.to_string_lossy(),
            "symbols": c.symbols,
            "intent": match c.intent { ClaimIntent::Read => "read", ClaimIntent::Edit => "edit" },
            "claimed_at": system_time_to_unix_secs(c.claimed_at),
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct ListSubagentsArgs {
    pub session_token: String,
}

/// `list_subagents` answers the parent agent's question: "which active
/// subagents are currently registered as mine?" The caller's session is
/// resolved via `session_token`; we then enumerate every active session
/// (including background agents — a parent may legitimately want to see
/// a cron-driven subagent too) and return those whose `parent_session_id`
/// matches the caller's `agent_id`. The JSON shape is the same one a
/// subagent sees on its own `who_am_i` minus `claims`, making it usable
/// as a lightweight rendering input from the parent's side.
pub fn run_list_subagents(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: ListSubagentsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let session = server.presence.by_token(&a.session_token).ok_or("unknown session token")?;
    let parent_id = session.id.clone();
    let mut children: Vec<Value> = Vec::new();
    for child in server.presence.list_active(true) {
        if child.parent_session_id.as_ref() == Some(&parent_id) {
            children.push(json!({
                "agent_id": child.id.as_str(),
                "name": child.name,
                "kind": child.kind.as_str(),
                "mode": child.mode.as_str(),
                "started_at_unix": system_time_to_unix_secs(child.started_at),
                "last_heartbeat_unix": system_time_to_unix_secs(child.last_heartbeat),
            }));
        }
    }
    Ok(json!({ "parent": parent_id.as_str(), "subagents": children }))
}

#[derive(Debug, Deserialize)]
pub struct ClaimFilesArgs {
    pub agent_id: String,
    pub session_token: String,
    pub files: Vec<ClaimFilesEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ClaimFilesEntry {
    pub path: String,
    pub symbols: Option<Vec<String>>,
    pub intent: Option<String>,
    /// Last plan revision the calling agent saw (Task 1.4, PR 1).
    /// `None` preserves the prior behavior for callers that don't
    /// track revisions yet.
    #[serde(default)]
    pub plan_revision: Option<crate::server::revision_log::RevisionId>,
}

pub fn run_claim_files(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: ClaimFilesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let session = server.presence.by_token(&a.session_token).ok_or("unknown session token")?;
    if session.id.as_str() != a.agent_id {
        return Err("agent_id does not match session token".into());
    }
    let requests: Vec<ClaimRequest> = a.files.into_iter().map(|f| ClaimRequest {
        path: std::path::PathBuf::from(f.path),
        symbols: f.symbols.unwrap_or_default(),
        intent: f.intent.as_deref().map(|s| if s == "read" { ClaimIntent::Read } else { ClaimIntent::Edit }).unwrap_or(ClaimIntent::Edit),
        ttl_seconds: None,
        plan_revision: f.plan_revision,
    }).collect();
    let result = server.occupancy.claim_with_session(&session, requests);
    if !result.granted.is_empty() {
        for g in &result.granted {
            let _ = server.presence_event_tx.send(PresenceEvent::ClaimGranted {
                agent_id: session.id.clone(),
                path: g.path.clone(),
            });
        }
    }
    if !result.conflicts.is_empty() {
        let _ = server.presence_event_tx.send(PresenceEvent::ConflictDetected {
            agent_id: session.id.clone(),
            conflicts: result.conflicts.clone(),
        });
    }
    let mut out = serde_json::Map::new();
    out.insert("granted".into(), Value::Array(result.granted.iter().map(|g| json!({
        "path": g.path.to_string_lossy(),
        "symbols": g.symbols,
    })).collect()));
    out.insert("conflicts".into(), Value::Array(result.conflicts.iter().map(|c| json!({
        "agent_id": c.agent_id.as_str(),
        "path": c.path.to_string_lossy(),
        "symbols": c.symbols,
        "intent": match c.intent { ClaimIntent::Read => "read", ClaimIntent::Edit => "edit" },
        "last_seen_unix": system_time_to_unix_secs(c.last_seen_unix),
    })).collect()));
    // `world_state` is populated by the static-graph retract detector
    // (Task 1.6, PR 1). When `None`, the field is omitted from the
    // wire response (matching the `skip_serializing_if` on the struct
    // field) so existing callers see no new shape.
    if let Some(ws) = result.world_state.as_ref() {
        if let Ok(v) = serde_json::to_value(ws) {
            out.insert("world_state".into(), v);
        }
    }
    Ok(Value::Object(out))
}

#[derive(Debug, Deserialize)]
pub struct ReleaseFilesArgs {
    pub agent_id: String,
    pub session_token: String,
    pub files: Vec<ReleaseFilesEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ReleaseFilesEntry {
    pub path: String,
    pub symbols: Option<Vec<String>>,
}

pub fn run_release_files(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: ReleaseFilesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let session = server.presence.by_token(&a.session_token).ok_or("unknown session token")?;
    if session.id.as_str() != a.agent_id {
        return Err("agent_id does not match session token".into());
    }
    let paths: Vec<std::path::PathBuf> = a.files.into_iter().map(|f| std::path::PathBuf::from(f.path)).collect();
    let released = server.occupancy.release(&session.id, &paths);
    for path in &released {
        let _ = server.presence_event_tx.send(PresenceEvent::ClaimReleased {
            agent_id: session.id.clone(),
            path: path.clone(),
        });
    }
    Ok(json!({ "released": released.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>() }))
}

#[derive(Debug, Deserialize)]
pub struct ListOccupancyArgs {
    pub path: Option<String>,
}

pub fn run_list_occupancy(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: ListOccupancyArgs = serde_json::from_value(args).unwrap_or(ListOccupancyArgs { path: None });
    let entries: Vec<OccupancyEntry> = if let Some(p) = a.path.as_deref() {
        server.occupancy.list_for_path(std::path::Path::new(p)).into_iter().collect()
    } else {
        server.occupancy.list_all()
    };
    let out: Vec<Value> = entries.into_iter().map(|e| {
        // `last_seen_unix` is the heartbeat of the first live agent
        // currently holding any claim on this path. The caller can
        // resolve the name via `list_active_agents` / `who_am_i` —
        // `agent_names` (which conflated "live" with "named") was
        // dropped because it would be silently empty for sessions
        // that had expired but still had artifacts on disk.
        let last_seen_unix: Option<u64> = e.agents.iter()
            .filter_map(|id| server.presence.get(id))
            .next()
            .map(|s| system_time_to_unix_secs(s.last_heartbeat));
        json!({
            "path": e.path.to_string_lossy(),
            "agents": e.agents.iter().map(|a| a.as_str()).collect::<Vec<_>>(),
            "last_seen_unix": last_seen_unix,
            "symbols": e.symbols.iter().map(|s| json!({
                "symbol": s.symbol,
                "agents": s.agents.iter().map(|a| a.as_str()).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }).collect();
    Ok(json!(out))
}

#[derive(Debug, Deserialize)]
pub struct MyClaimsArgs {
    pub agent_id: String,
    pub session_token: String,
}

pub fn run_my_claims(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: MyClaimsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let session = server.presence.by_token(&a.session_token).ok_or("unknown session token")?;
    if session.id.as_str() != a.agent_id {
        return Err("agent_id does not match session token".into());
    }
    let claims = server.occupancy.list_for_agent(&session.id);
    Ok(json!(claims.into_iter().map(|c| json!({
        "path": c.path.to_string_lossy(),
        "symbols": c.symbols,
        "intent": match c.intent { ClaimIntent::Read => "read", ClaimIntent::Edit => "edit" },
        "claimed_at": system_time_to_unix_secs(c.claimed_at),
    })).collect::<Vec<_>>()))
}

fn system_time_to_unix_secs(t: std::time::SystemTime) -> u64 {
    t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn system_time_to_unix_secs_delta(d: std::time::Duration) -> u64 { d.as_secs() }

// ── detect_overlap ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DetectOverlapArgs {
    /// Ref to compare against — normally the merge target's tip or the
    /// merge base.
    pub base: String,
    /// Ref carrying the local work. Defaults to `HEAD`.
    pub head: Option<String>,
    /// Federation workspace whose member repos are scanned.
    pub workspace: String,
}

/// Compare the symbol sets of two git refs and report what both sides
/// touched.
///
/// Unlike `list_occupancy`, which answers "who is in this file *right
/// now*", this is the commit-time check: for every file that differs
/// between `base` and `head`, extract the symbols defined at each ref and
/// return the intersection. A non-empty intersection means the two refs
/// edited the same definition and a textual merge is likely to either
/// conflict or silently drop one side's change.
///
/// Symbols come from tree-sitter (`extract_definitions`) run over the file
/// content at each ref, not from the federation graph — the graph only
/// holds the indexed working state, so it cannot answer "what did this
/// file look like at `base`".
///
/// Each file carries a `severity` of `"none"` / `"low"` / `"medium"` /
/// `"high"`, graded by `overlap_severity` from the kinds of the shared
/// symbols rather than merely by whether any were shared.
pub fn run_detect_overlap(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: DetectOverlapArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let head = a.head.clone().unwrap_or_else(|| "HEAD".to_string());

    let fed = server
        .federation()
        .ok_or("federation not configured on this server")?;
    let workspaces = server
        .workspaces_snapshot()
        .ok_or("no workspaces file loaded on this server")?;
    let spec = workspaces
        .workspaces
        .iter()
        .find(|w| w.name == a.workspace)
        .ok_or_else(|| format!("unknown workspace {}", a.workspace))?;

    // Resolve every member to its on-disk worktree root. Members listed in
    // workspaces.yaml but not loaded into the federation are skipped —
    // there is no path to run git in.
    let mut roots: Vec<(String, std::path::PathBuf)> = Vec::new();
    for member in &spec.members {
        let Ok(id) = crate::federation::repo_id::RepoId::new(member) else {
            continue;
        };
        if let Some(repo) = fed.get_repo(&id) {
            roots.push((member.clone(), repo.source().local_path().to_path_buf()));
        }
    }
    if roots.is_empty() {
        return Err(format!(
            "workspace {} has no loaded member repos",
            a.workspace
        ));
    }

    let mut out_files: Vec<Value> = Vec::new();
    let mut total_overlaps = 0usize;
    let mut diff_errors: Vec<String> = Vec::new();

    for (repo_id, root) in &roots {
        let paths = match git_diff_names(root, &a.base, &head) {
            Ok(p) => p,
            // A ref that resolves in one member repo usually does not
            // resolve in the others, so a failed diff is the normal case
            // for a multi-repo workspace. Record it and keep going; only a
            // workspace where *every* member failed is a hard error.
            Err(e) => {
                diff_errors.push(format!("{repo_id}: {e}"));
                continue;
            }
        };
        for path in paths {
            let defs_base = symbols_at_ref(root, &a.base, &path);
            let defs_head = symbols_at_ref(root, &head, &path);
            // Overlap is matched on the symbol *name* (as before); the kind is
            // carried along only to weight the severity. The base side's kind
            // wins when a name changed kind between refs — that rename is
            // itself the riskier reading.
            let head_names: std::collections::HashSet<&str> =
                defs_head.iter().map(|(n, _)| n.as_str()).collect();
            let overlap: Vec<(String, NodeType)> = defs_base
                .iter()
                .filter(|(n, _)| head_names.contains(n.as_str()))
                .cloned()
                .collect();
            total_overlaps += overlap.len();
            out_files.push(json!({
                "repo": repo_id,
                "path": path,
                "symbols_base": defs_base.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
                "symbols_head": defs_head.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
                "severity": overlap_severity(&overlap),
                "overlap": overlap.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            }));
        }
    }

    if out_files.is_empty() && diff_errors.len() == roots.len() {
        return Err(format!(
            "git diff {}..{} failed in every member repo: {}",
            a.base,
            head,
            diff_errors.join("; ")
        ));
    }

    Ok(json!({
        "base": a.base,
        "head": head,
        "files": out_files,
        "total_overlaps": total_overlaps,
    }))
}

/// How much a single shared symbol contributes to a file's severity score.
///
/// The ranking is "how likely is a textual merge to silently lose one side's
/// work": bodies of functions and methods carry the most logic, type
/// definitions next, fields/properties after that, and containers/imports
/// least. Every kind is worth at least 1, so a non-empty overlap can never
/// score 0 and be mistaken for "none".
fn symbol_weight(kind: &NodeType) -> u32 {
    match kind {
        // Behaviour: two refs editing the same body is the classic silent-drop.
        NodeType::Function | NodeType::Method => 4,
        // Type definitions: a shared shape usually means a shared contract.
        NodeType::Struct | NodeType::Enum | NodeType::Trait | NodeType::Class
        | NodeType::Interface | NodeType::Schema => 3,
        // Members: narrower blast radius than a whole type.
        NodeType::Property | NodeType::Variable => 2,
        // Containers, constants, imports and cross-runtime markers: usually
        // co-edited incidentally.
        NodeType::File | NodeType::Namespace | NodeType::Module | NodeType::Package
        | NodeType::Constant | NodeType::HttpRoute | NodeType::Topic
        | NodeType::Resource => 1,
    }
}

/// Graduated severity for one file's symbol overlap.
///
/// `"none"` when nothing is shared. Otherwise the per-symbol weights from
/// `symbol_weight` (1–4) are summed:
///
/// - `>= 6` → `"high"`: at least two behaviour-carrying symbols (4+4, 4+3,
///   3+3), or a long tail of smaller ones — a merge here needs eyes on it.
/// - `>= 3` → `"medium"`: one shared function/type (4 or 3), or a pair of
///   members (2+2).
/// - otherwise → `"low"`: one or two incidental symbols (1, 2, 1+1).
///
/// The value space is additive: `"none"` and `"high"` keep their original
/// meaning for existing callers, and `"low"` / `"medium"` slot in between.
fn overlap_severity(overlap: &[(String, NodeType)]) -> &'static str {
    if overlap.is_empty() {
        return "none";
    }
    let weighted: u32 = overlap.iter().map(|(_, kind)| symbol_weight(kind)).sum();
    if weighted >= 6 {
        "high"
    } else if weighted >= 3 {
        "medium"
    } else {
        // Weights are all >= 1, so a non-empty overlap lands here only for
        // genuinely small scores — but this arm is also the defensive floor
        // that keeps a non-empty overlap from ever reporting "none".
        "low"
    }
}

/// `git diff --name-only <base> <head>` in `root`, one repo-relative path
/// per line. The two-argument form (rather than `<base>..<head>`) is used
/// so a ref containing `..` cannot be misparsed as a range.
fn git_diff_names(
    root: &std::path::Path,
    base: &str,
    head: &str,
) -> Result<Vec<String>, String> {
    let out = std::process::Command::new("git")
        .current_dir(root)
        .args(["diff", "--name-only", base, head])
        .output()
        .map_err(|e| format!("git diff failed to start: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Sorted, de-duplicated `(name, kind)` pairs for every symbol tree-sitter
/// can see in `path` as of `git_ref`. An empty vec covers three cases that
/// all mean "nothing to overlap here": the file did not exist at that ref, it
/// was unreadable or binary, or its language has no extractor.
///
/// The kind rides along so `overlap_severity` can weight a shared function
/// more heavily than a shared module. De-duplication is by name only: two
/// definitions sharing a name in one file (a `struct Foo` plus its `impl`-block
/// helpers, say) collapse to the first kind seen after the sort.
fn symbols_at_ref(
    root: &std::path::Path,
    git_ref: &str,
    path: &str,
) -> Vec<(String, NodeType)> {
    let Ok(out) = std::process::Command::new("git")
        .current_dir(root)
        .args(["show", &format!("{git_ref}:{path}")])
        .output()
    else {
        return vec![];
    };
    if !out.status.success() {
        return vec![];
    }
    let Ok(source) = String::from_utf8(out.stdout) else {
        return vec![];
    };
    let mut defs: Vec<(String, NodeType)> =
        crate::server::treesitter::extract_definitions(std::path::Path::new(path), &source)
            .into_iter()
            .map(|d| (d.name, d.kind))
            .collect();
    defs.sort_by(|a, b| a.0.cmp(&b.0));
    defs.dedup_by(|a, b| a.0 == b.0);
    defs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::schema::NodeType;

    /// Pins every band of `overlap_severity` directly. Lives in a unit
    /// test (rather than the integration test file) so `overlap_severity`
    /// can stay private; integration tests don't need to call it because
    /// the real `detect_overlap` MCP tool already exercises every band
    /// end-to-end (`detect_overlap_reports_shared_symbols` for
    /// medium/high, `detect_overlap_two_shared_functions_is_high` for
    /// high, and the empty-overlap case for none).
    #[test]
    fn overlap_severity_bands() {
        // Empty overlap → none.
        assert_eq!(overlap_severity(&[]), "none");

        // Weight 1 and 2 → low.
        assert_eq!(
            overlap_severity(&[("logging".to_string(), NodeType::Module)]),
            "low"
        );
        assert_eq!(
            overlap_severity(&[("timeout_ms".to_string(), NodeType::Property)]),
            "low"
        );
        assert_eq!(
            overlap_severity(&[
                ("logging".to_string(), NodeType::Module),
                ("MAX".to_string(), NodeType::Constant),
            ]),
            "low"
        );

        // Weight 3–5 → medium: one type, one function, or a pair of members.
        assert_eq!(
            overlap_severity(&[("Config".to_string(), NodeType::Struct)]),
            "medium"
        );
        assert_eq!(
            overlap_severity(&[("login".to_string(), NodeType::Function)]),
            "medium"
        );
        assert_eq!(
            overlap_severity(&[
                ("a".to_string(), NodeType::Property),
                ("b".to_string(), NodeType::Variable),
            ]),
            "medium"
        );

        // Weight >= 6 → high.
        assert_eq!(
            overlap_severity(&[
                ("login".to_string(), NodeType::Function),
                ("logout".to_string(), NodeType::Function),
            ]),
            "high"
        );
        assert_eq!(
            overlap_severity(&[
                ("login".to_string(), NodeType::Function),
                ("Config".to_string(), NodeType::Struct),
            ]),
            "high"
        );
    }
}