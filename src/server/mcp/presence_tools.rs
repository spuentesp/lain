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
    AgentId, AgentKind, AgentMode, AgentSession, ChangedKind, ChangedSymbol, ClaimIntent,
    ClaimRequest, OccupancyEntry, PresenceEvent, WorldState,
};
use crate::server::revision_log::{LookupResult, RevisionId};
use crate::server::schema::NodeType;
use crate::server::audit::{append_edit_event, AuditEvent};
use crate::server::path_util::posix_string;
use serde::Deserialize;
use std::path::PathBuf;
use serde_json::{json, Value};

/// Resolve a session token to its session, refreshing the heartbeat as
/// a side effect.
///
/// Sessions used to expire on wall clock alone: only an explicit
/// `heartbeat` reset the timer, so an agent that claimed a file,
/// thought for a minute and came back found its session gone — and with
/// it every claim it held, silently, leaving the file free for the next
/// agent to take mid-edit. An LLM agent has no timer between turns, so
/// "call heartbeat every N seconds" is not something it can honor.
///
/// Any authenticated call is proof of life. Every dispatcher that
/// resolves a token goes through here.
/// Resolve an agent id to its registered name.
///
/// Conflicts and occupancy reported raw UUIDs, so an agent that was told
/// "someone else holds this" had to make a second `list_active_agents`
/// round-trip to learn who — and a human reading the dashboard saw a
/// hex string. `None` when the session has since expired, which is
/// itself useful: the holder is gone.
fn agent_name(server: &LainServer, id: &AgentId) -> Option<String> {
    server.presence.get(id).map(|s| s.name)
}

fn authenticate(server: &LainServer, token: &str) -> Result<AgentSession, String> {
    let session = server.presence.by_token(token).ok_or("unknown session token")?;
    // Best-effort: the session was just resolved, so this only fails on
    // a race with expiry, in which case the caller's own work still
    // proceeds on the session it already holds.
    let _ = server.presence.heartbeat(&session.id, token);
    Ok(session)
}

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
    server.with_shared_presence(|| run_register_agent_inner(server, a))
}

fn run_register_agent_inner(server: &LainServer, a: RegisterAgentArgs) -> Result<Value, String> {
    let kind = a.kind.as_deref().map(AgentKind::parse).unwrap_or(AgentKind::Other("unknown".into()));
    let mode = a.mode.as_deref().map(AgentMode::parse).unwrap_or(AgentMode::Interactive);
    let parent = a.parent_session_id.map(AgentId);
    let session = server.presence.register(a.name, kind, mode, a.pid, parent);
    server.emit_presence_event(PresenceEvent::AgentJoined(session.clone()));
    // Report the TTL this session actually gets, not the registry
    // default — background agents are reaped faster than interactive
    // ones, and an agent that plans around the wrong number loses its
    // claims without warning.
    let expires_at_unix = crate::server::time::unix_secs_u64(session.started_at)
        + server.presence.expires_after_for(&session.mode).as_secs();
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
    server.with_shared_presence(|| run_heartbeat_inner(server, a))
}

fn run_heartbeat_inner(server: &LainServer, a: HeartbeatArgs) -> Result<Value, String> {
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
    // A listing that only sees this process's own memory is the
    // symptom that made the split invisible.
    server.refresh_shared_presence();
    let a: ListActiveAgentsArgs = serde_json::from_value(args).unwrap_or(ListActiveAgentsArgs { include_background: None });
    let sessions = server.presence.list_active(a.include_background.unwrap_or(false));
    let out: Vec<Value> = sessions.into_iter().map(|s| {
        let claims = server.occupancy.list_for_agent(&s.id);
        json!({
            "agent_id": s.id.as_str(),
            "name": s.name,
            "kind": s.kind.as_str(),
            "mode": s.mode.as_str(),
            "started_at": crate::server::time::unix_secs_u64(s.started_at),
            "last_heartbeat": crate::server::time::unix_secs_u64(s.last_heartbeat),
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
    server.refresh_shared_presence();
    let a: WhoAmIArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let session = authenticate(server, &a.session_token)?;
    let claims = server.occupancy.list_for_agent(&session.id);
    let parent_session_id = session.parent_session_id.as_ref().map(|p| p.as_str().to_string());
    Ok(json!({
        "agent_id": session.id.as_str(),
        "name": session.name,
        "kind": session.kind.as_str(),
        "mode": session.mode.as_str(),
        "parent_session_id": parent_session_id,
        "claims": claims.into_iter().map(|c| json!({
            "path": posix_string(&c.path),
            "symbols": c.symbols,
            "intent": match c.intent { ClaimIntent::Read => "read", ClaimIntent::Edit => "edit" },
        "inferred": c.inferred,
            "claimed_at": crate::server::time::unix_secs_u64(c.claimed_at),
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
    server.refresh_shared_presence();
    let a: ListSubagentsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let session = authenticate(server, &a.session_token)?;
    let parent_id = session.id.clone();
    let mut children: Vec<Value> = Vec::new();
    for child in server.presence.list_active(true) {
        if child.parent_session_id.as_ref() == Some(&parent_id) {
            children.push(json!({
                "agent_id": child.id.as_str(),
                "name": child.name,
                "kind": child.kind.as_str(),
                "mode": child.mode.as_str(),
                "started_at_unix": crate::server::time::unix_secs_u64(child.started_at),
                "last_heartbeat_unix": crate::server::time::unix_secs_u64(child.last_heartbeat),
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

#[derive(Debug)]
pub struct ClaimFilesEntry {
    pub path: String,
    pub symbols: Option<Vec<String>>,
    pub intent: Option<String>,
    /// Last plan revision the calling agent saw (Task 1.4, PR 1).
    /// `None` preserves the prior behavior for callers that don't
    /// track revisions yet.
    pub plan_revision: Option<crate::server::revision_log::RevisionId>,
}

/// Accept both `"src/a.rs"` and `{"path": "src/a.rs"}`.
///
/// Mirrors `ReleaseFilesEntry` (above). A claim entry carries
/// `intent` and `ttl_seconds`; the string form has nothing to set on
/// those, so the bare path is the obvious spelling and an agent
/// writes it without thinking. It used to be rejected with `invalid
/// type: string "src/a.rs", expected struct ClaimFilesEntry` — a
/// Rust type name the caller cannot act on, for input that was never
/// ambiguous.
impl<'de> serde::Deserialize<'de> for ClaimFilesEntry {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct AsObject {
            path: String,
            symbols: Option<Vec<String>>,
            intent: Option<String>,
            #[serde(default)]
            plan_revision: Option<crate::server::revision_log::RevisionId>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Either {
            Path(String),
            Object(AsObject),
        }
        Ok(match Either::deserialize(d)? {
            Either::Path(path) => ClaimFilesEntry {
                path,
                symbols: None,
                intent: None,
                plan_revision: None,
            },
            Either::Object(o) => ClaimFilesEntry {
                path: o.path,
                symbols: o.symbols,
                intent: o.intent,
                plan_revision: o.plan_revision,
            },
        })
    }
}

// ─── get_world_state (P0 gap fix) ─────────────────────────────────────
// Read-only companion to claim_files. Surfaces the same `WorldState`
// shape (retract detection, overlay-delta filtering) without requiring
// the agent to take a claim. Lets an LLM ask "is this symbol still in
// the graph?" or "what's the current world state for these symbols?"
// before deciding whether to claim, edit, or skip.
#[derive(Debug, Deserialize)]
pub struct GetWorldStateArgs {
    /// Symbols to inspect for retract detection. Empty list returns
    /// a `WorldState` with empty `changed_symbols` and `note: None`
    /// (no-op query).
    #[serde(default)]
    pub symbols: Vec<String>,
    /// Last plan revision the calling agent saw. `None` uses the
    /// current overlay revision (no-op query, no resync note).
    #[serde(default)]
    pub plan_revision: Option<crate::server::revision_log::RevisionId>,
}

pub fn run_get_world_state(
    server: &LainServer,
    args: Value,
) -> Result<Value, String> {
    let a: GetWorldStateArgs =
        serde_json::from_value(args).map_err(|e| e.to_string())?;
    let plan = a.plan_revision.unwrap_or_else(|| server.overlay.current_revision());
    let ws = compute_world_state(server, plan, &a.symbols, &std::collections::HashSet::new());
    serde_json::to_value(ws).map_err(|e| e.to_string())
}

pub fn run_claim_files(server: &LainServer, args: Value) -> Result<Value, String> {
    // Say what the tool accepts. The bare serde message names internal
    // Rust types, which tells an agent nothing it can fix.
    let a: ClaimFilesArgs = serde_json::from_value(args).map_err(|e| {
        format!(
            "claim_files: {e}. `files` is a list of paths — either \
             [\"src/a.rs\"] or [{{\"path\": \"src/a.rs\"}}] — plus \
             `agent_id` and `session_token`."
        )
    })?;
    // Conflict detection is the whole point of this tool, and it can
    // only see peers if the registry is refreshed from the shared state
    // file first — and only stay correct if the grant is written back
    // under the same lock.
    server.with_shared_presence(|| run_claim_files_inner(server, a))
}

fn run_claim_files_inner(server: &LainServer, a: ClaimFilesArgs) -> Result<Value, String> {
    let session = authenticate(server, &a.session_token)?;
    if session.id.as_str() != a.agent_id {
        return Err("agent_id does not match session token".into());
    }
    // Capture `plan_revision` and the union of requested symbols up
    // front so we can populate `world_state` after the claim is
    // granted. Per Task 1.4, `plan_revision` travels per-file; we
    // take the first non-None across the request set, matching the
    // spec's "Some(_) when the request included a plan_revision"
    // contract. If no file carried a revision, the response stays
    // world_state-less for legacy callers.
    let plan_revision: Option<RevisionId> = a.files.iter().find_map(|f| f.plan_revision);
    let requested_symbols: Vec<String> = a.files.iter()
        .flat_map(|f| f.symbols.clone().unwrap_or_default())
        .collect();
    let requests: Vec<ClaimRequest> = a.files.into_iter().map(|f| ClaimRequest {
        path: std::path::PathBuf::from(f.path),
        symbols: f.symbols.unwrap_or_default(),
        intent: f.intent.as_deref().map(|s| if s == "read" { ClaimIntent::Read } else { ClaimIntent::Edit }).unwrap_or(ClaimIntent::Edit),
        ttl_seconds: None,
        plan_revision: f.plan_revision,
    }).collect();
    // Snapshot the agent's held symbols *before* the claim lands.
    // Retract detection asks "was this symbol here when you last
    // looked?", and after the claim is applied every symbol in the
    // request is trivially held — which would make every unknown name
    // look like a retraction.
    let held_before: std::collections::HashSet<String> = server
        .occupancy
        .list_for_agent(&session.id)
        .into_iter()
        .flat_map(|c| c.symbols)
        .collect();
    let mut result = server.occupancy.claim_with_session(&session, requests);
    // Populate `world_state` only when the caller supplied a
    // `plan_revision`. The brief's Step 3 pseudocode:
    //   1. Check each requested symbol against the static graph and
    //      record `Retracted` entries for symbols not present.
    //   2. Ask the overlay for `diffs_since(plan)`. Three branches:
    //      `Ok(diffs)` → fold into `Edited` entries via
    //      `ChangedSymbol::from_diffs`; `BeyondCurrent` / `TooOld`
    //      → empty `changed_symbols` plus a `note` for the agent.
    //   3. Combine the two sources and emit `Some(WorldState)`.
    if let Some(plan) = plan_revision {
        result.world_state = Some(compute_world_state(server, plan, &requested_symbols, &held_before));
    }
    if !result.granted.is_empty() {
        for g in &result.granted {
            server.emit_presence_event(PresenceEvent::ClaimGranted {
                agent_id: session.id.clone(),
                path: g.path.clone(),
            });
        }
        // Audit append (Task 2.3, PR 2). The spec's invariant is
        // "audit never blocks an edit": one event per granted path,
        // `racers` populated from the post-resolution conflict list
        // (empty when uncontested), `landed_revision` captured
        // *after* `OccupancyMap::claim` so it reflects the
        // post-claim overlay state the agent believes it is writing
        // into. The append is best-effort: a filesystem failure
        // emits `WARN` and the claim itself remains valid. The
        // `claim_set` is the post-grant `Claim` snapshot pulled
        // from the occupancy map — the spec records the claims the
        // writer *believes itself to hold*, not the request payload.
        // We snapshot the agent's full claim set once and partition
        // it per granted path so a multi-file claim still produces
        // one audit line per granted file.
        let all_claims = server.occupancy.list_for_agent(&session.id);
        let landed_revision = server.overlay.current_revision();
        let audit_dir = server.state_dir_for_audit();
        let ts_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        for g in &result.granted {
            let claim_set: Vec<crate::server::presence::Claim> = all_claims
                .iter()
                .filter(|c| c.path == g.path)
                .cloned()
                .collect();
            let audit = AuditEvent {
                ts_unix,
                agent_id: session.id.clone(),
                // Store the path in the canonical forward-slash form so
                // the on-disk JSONL is platform-independent. Any
                // `path_glob` filter that uses `/` (see audit_tools.rs)
                // will match regardless of host OS, and downstream
                // consumers (federation replication, future log
                // shipping) see a stable wire form.
                path: posix_string(&g.path),
                claim_set,
                racers: result.conflicts.clone(),
                plan_revision,
                landed_revision,
            };
            if let Err(e) = append_edit_event(&audit_dir, &audit) {
                tracing::warn!("audit append failed: {e}");
            }
            // SSE `edit_landed` (PR 2 / Task 2.4) — same best-effort
            // contract as the audit append: a dropped subscriber or a
            // closed broadcast channel must never block the claim. The
            // wire payload is the same `AuditEvent` we just wrote to
            // disk, so Command Center subscribers see the write the
            // instant it lands rather than waiting for a future
            // `get_audit_log` poll.
            server.emit_presence_event(PresenceEvent::EditLanded { event: audit.clone() });
        }
    }
    if !result.conflicts.is_empty() {
        let severity = runtime_conflict_severity(server, &result.conflicts);
        server.emit_presence_event(PresenceEvent::ConflictDetected {
            agent_id: session.id.clone(),
            conflicts: result.conflicts.clone(),
            severity: severity.to_string(),
        });
    }
    let mut out = serde_json::Map::new();
    out.insert("granted".into(), Value::Array(result.granted.iter().map(|g| json!({
        "path": posix_string(&g.path),
        "symbols": g.symbols,
    })).collect()));
    out.insert("conflicts".into(), Value::Array(result.conflicts.iter().map(|c| json!({
        "agent_id": c.agent_id.as_str(),
        "name": agent_name(server, &c.agent_id),
        "path": posix_string(&c.path),
        "symbols": c.symbols,
        "intent": match c.intent { ClaimIntent::Read => "read", ClaimIntent::Edit => "edit" },
        "inferred": c.inferred,
        "last_seen_unix": crate::server::time::unix_secs_u64(c.last_seen_unix),
    })).collect()));
    // Advisories: granted, but somebody else is editing this file.
    // Omitted when empty so unchanged responses keep their shape.
    if !result.advisories.is_empty() {
        out.insert("advisories".into(), Value::Array(result.advisories.iter().map(|c| json!({
            "agent_id": c.agent_id.as_str(),
            "name": agent_name(server, &c.agent_id),
            "path": posix_string(&c.path),
            "symbols": c.symbols,
            "intent": match c.intent { ClaimIntent::Read => "read", ClaimIntent::Edit => "edit" },
            "inferred": c.inferred,
            "last_seen_unix": crate::server::time::unix_secs_u64(c.last_seen_unix),
            "note": "granted — another agent holds an edit claim here; re-read before you patch",
        })).collect()));
    }
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

/// Build the `WorldState` payload for a `claim_files` call that
/// supplied a `plan_revision`. Three layers:
///
/// 1. **Retracted** — any requested symbol the static graph no longer
///    resolves. Federation mode consults the `GraphBackend` name index
///    (matches `project_repo`'s retract-aware contract); single-workspace
///    servers consult the per-repo `GraphDatabase`.
/// 2. **Edited** — symbols touched by overlay diffs since `plan`.
///    `ChangedSymbol::from_diffs` dedups by name and keeps the latest
///    `at_revision`. Filtered to the requested symbols so unrelated
///    overlay churn doesn't pollute the response (spec: "filters to
///    the claim's paths only").
/// 3. **Note** — set on `BeyondCurrent` (the world moved past the
///    caller's plan before claim landed) and `TooOld` (the plan is
///    older than the ring buffer's floor). The agent uses the note to
///    decide whether to re-query or to proceed under advisory.
fn compute_world_state(
    server: &LainServer,
    plan: RevisionId,
    requested_symbols: &[String],
    // `held_before`: symbols the caller already held *before* this
    // call. A symbol in there was in the graph when its claim was
    // granted, so its absence now is a genuine retraction; anything
    // else was simply never indexed.
    held_before: &std::collections::HashSet<String>,
) -> WorldState {
    let current = server.overlay.current_revision();

    // ── (a) Static-graph retract detection ─────────────────────────────
    // The brief's pseudocode names `FederatedIndex::symbol_to_repos`
    // (the in-memory name index) and `GraphDatabase::find_nodes_by_name`
    // (per-repo). The federation's `symbol_to_repos` is private, so we
    // call the public `GraphBackend::find_nodes_by_name` instead — the
    // same name it uses for the `resolve_symbol` fallback path. The
    // per-repo `GraphDatabase` exposes `find_node_by_name` (singular),
    // which is sufficient for the "does any node have this name?"
    // question; we use the singular form accordingly.
    // "Absent" alone cannot distinguish *removed* from *never present*.
    // The caller's own claim history can: a symbol this agent already
    // holds a claim on was in the graph when the claim was granted, so
    // its disappearance is a genuine retraction. Anything else is
    // simply not indexed — which is what an agent asking about a match
    // arm, or a function added since the last index, should be told.
    let mut retracted: Vec<ChangedSymbol> = Vec::new();
    for sym in requested_symbols {
        if !symbol_exists_in_static_graph(server, sym) {
            retracted.push(ChangedSymbol {
                name: sym.clone(),
                change_kind: if held_before.contains(sym) {
                    ChangedKind::Retracted
                } else {
                    ChangedKind::NotIndexed
                },
                at_revision: current,
            });
        }
    }

    // ── (b) Overlay diffs since `plan` ─────────────────────────────────
    let overlay_diffs = match server.overlay.diffs_since(plan) {
        Ok(ds) => ds,
        Err(err) => match err {
            LookupResult::BeyondCurrent => {
                // `retracted` is independent of the overlay revision
                // counter: the static graph can drop a symbol while
                // the overlay hasn't moved. Keep the retract set even
                // when the overlay window is unreachable — the agent
                // still benefits from knowing the static graph no
                // longer has the symbol they queried for.
                return WorldState {
                    current,
                    plan,
                    changed_symbols: retracted,
                    note: Some(
                        "plan_revision beyond current — server may have restarted".into(),
                    ),
                };
            }
            LookupResult::TooOld => {
                return WorldState {
                    current,
                    plan,
                    changed_symbols: retracted,
                    note: Some("plan_revision too old for delta; resync required".into()),
                };
            }
            // `LookupResult::Ok` is the (within-window) success arm of
            // the enum, but here we're already in the `Err` branch of
            // `diffs_since` — `Ok` is unreachable. Explicitly handle
            // it so the match stays exhaustive against future enum
            // growth (e.g. a `Transitional` variant that wouldn't be
            // an error).
            LookupResult::Ok => Vec::new(),
        },
    };

    // ── (c) Combine overlay `Edited` entries with the retract set ──────
    // Filter the overlay diffs to symbols the claim actually asked
    // about; otherwise a single claim would surface every overlay
    // mutation since `plan`, which is noise an agent has to ignore.
    let mut changed_symbols = ChangedSymbol::from_diffs(&overlay_diffs, plan, current);
    if !requested_symbols.is_empty() {
        let requested: std::collections::HashSet<&str> =
            requested_symbols.iter().map(|s| s.as_str()).collect();
        changed_symbols.retain(|cs| requested.contains(cs.name.as_str()));
    }
    changed_symbols.extend(retracted);
    WorldState {
        current,
        plan,
        changed_symbols,
        note: None,
    }
}

/// Whether `sym` is currently a node in the static graph. Federation
/// mode routes through the federation's `GraphBackend` (the same
/// surface `project_repo` uses for retracts); single-workspace mode
/// checks the per-repo `GraphDatabase`. Errors are treated as
/// "symbol not present" — the retract detector is best-effort and
/// must not block a claim.
fn symbol_exists_in_static_graph(server: &LainServer, sym: &str) -> bool {
    if let Some(fed) = server.federation() {
        match fed.backend().find_nodes_by_name(sym) {
            Ok(nodes) => !nodes.is_empty(),
            Err(_) => false,
        }
    } else {
        server.graph.find_node_by_name(sym).is_some()
    }
}

#[derive(Debug, Deserialize)]
pub struct ReleaseFilesArgs {
    pub agent_id: String,
    pub session_token: String,
    pub files: Vec<ReleaseFilesEntry>,
}

#[derive(Debug)]
pub struct ReleaseFilesEntry {
    pub path: String,
    pub symbols: Option<Vec<String>>,
}

/// Accept both `"src/a.rs"` and `{"path": "src/a.rs"}`.
///
/// `claim_files` needs objects because a claim carries `intent` and
/// `ttl_seconds`; a release carries neither, so the bare path is the
/// obvious spelling and an agent writes it without thinking. It used to
/// be rejected with `invalid type: string "src/a.rs", expected struct
/// ReleaseFilesEntry` — a Rust type name the caller cannot act on, for
/// input that was never ambiguous.
impl<'de> serde::Deserialize<'de> for ReleaseFilesEntry {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct AsObject {
            path: String,
            symbols: Option<Vec<String>>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Either {
            Path(String),
            Object(AsObject),
        }
        Ok(match Either::deserialize(d)? {
            Either::Path(path) => ReleaseFilesEntry {
                path,
                symbols: None,
            },
            Either::Object(o) => ReleaseFilesEntry {
                path: o.path,
                symbols: o.symbols,
            },
        })
    }
}

pub fn run_release_files(server: &LainServer, args: Value) -> Result<Value, String> {
    // Say what the tool accepts. The bare serde message names internal
    // Rust types, which tells an agent nothing it can fix.
    let a: ReleaseFilesArgs = serde_json::from_value(args).map_err(|e| {
        format!(
            "release_files: {e}. `files` is a list of paths — either \
             [\"src/a.rs\"] or [{{\"path\": \"src/a.rs\"}}] — plus \
             `agent_id` and `session_token`."
        )
    })?;
    server.with_shared_presence(|| run_release_files_inner(server, a))
}

fn run_release_files_inner(server: &LainServer, a: ReleaseFilesArgs) -> Result<Value, String> {
    let session = authenticate(server, &a.session_token)?;
    if session.id.as_str() != a.agent_id {
        return Err("agent_id does not match session token".into());
    }
    let paths: Vec<std::path::PathBuf> = a.files.into_iter().map(|f| std::path::PathBuf::from(f.path)).collect();
    let released = server.occupancy.release(&session.id, &paths);
    for path in &released {
        server.emit_presence_event(PresenceEvent::ClaimReleased {
            agent_id: session.id.clone(),
            path: path.clone(),
        });
    }
    Ok(json!({ "released": released.iter().map(|p| posix_string(p)).collect::<Vec<_>>() }))
}

#[derive(Debug, Deserialize)]
pub struct ListOccupancyArgs {
    pub path: Option<String>,
}

pub fn run_list_occupancy(server: &LainServer, args: Value) -> Result<Value, String> {
    server.refresh_shared_presence();
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
            .map(|s| crate::server::time::unix_secs_u64(s.last_heartbeat));
        json!({
            "path": posix_string(&e.path),
            "agents": e.agents.iter().map(|a| a.as_str()).collect::<Vec<_>>(),
            // `holders` says *how* each agent holds the path. Without
            // it a surveying agent cannot tell a blocking `edit` from a
            // harmless `read` and has to attempt a claim to find out.
            "holders": e.holders.iter().map(|h| json!({
                "agent_id": h.agent_id.as_str(),
                "name": agent_name(server, &h.agent_id),
                "intent": match h.intent { ClaimIntent::Read => "read", ClaimIntent::Edit => "edit" },
                "inferred": h.inferred,
            })).collect::<Vec<_>>(),
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
    server.refresh_shared_presence();
    let a: MyClaimsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let session = authenticate(server, &a.session_token)?;
    if session.id.as_str() != a.agent_id {
        return Err("agent_id does not match session token".into());
    }
    let claims = server.occupancy.list_for_agent(&session.id);
    Ok(json!(claims.into_iter().map(|c| json!({
        "path": posix_string(&c.path),
        "symbols": c.symbols,
        "intent": match c.intent { ClaimIntent::Read => "read", ClaimIntent::Edit => "edit" },
        "inferred": c.inferred,
        "claimed_at": crate::server::time::unix_secs_u64(c.claimed_at),
    })).collect::<Vec<_>>()))
}

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
        .ok_or(
            "detect_overlap needs federation mode: start the server with \
             `lain server --config repos.yaml`. This process was started \
             without a federation config, so it has no repos to scan.",
        )?;
    let workspaces = server
        .workspaces_snapshot()
        .ok_or(
            "detect_overlap needs a workspaces file, and this server has none \
             loaded. Create one with `lain workspaces create <name> --members \
             <repo-ids>`, then restart the server or call `request_reload`.",
        )?;
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

/// Classify a live occupancy conflict with the same weighted bands used by
/// `detect_overlap`. Symbol claims are resolved through the current graph so
/// their `NodeType` contributes the same weight. File-level claims have no
/// symbols to resolve, so each distinct path contributes the minimum weight.
fn runtime_conflict_severity(
    server: &LainServer,
    conflicts: &[crate::server::presence::ConflictEntry],
) -> &'static str {
    let mut seen_symbols = std::collections::HashSet::new();
    let mut overlap = Vec::new();

    for symbol in conflicts.iter().flat_map(|conflict| &conflict.symbols) {
        if !seen_symbols.insert(symbol.as_str()) {
            continue;
        }
        let kind = if let Some(fed) = server.federation() {
            fed.backend()
                .find_nodes_by_name(symbol)
                .ok()
                .and_then(|nodes| nodes.into_iter().next())
                .map(|node| node.node_type)
        } else {
            server.graph.find_node_by_name(symbol).map(|node| node.node_type)
        }
        // A live claim may name a symbol not yet indexed. Treat it as a member
        // rather than dropping it from the score: the conflict is still real.
        .unwrap_or(NodeType::Variable);
        overlap.push((symbol.clone(), kind));
    }

    if overlap.is_empty() {
        let mut paths = std::collections::HashSet::new();
        overlap.extend(conflicts.iter().filter_map(|conflict| {
            paths
                .insert(conflict.path.clone())
                .then(|| (posix_string(&conflict.path), NodeType::File))
        }));
    }

    overlap_severity(&overlap)
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
    use std::path::PathBuf;

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

    #[test]
    fn posix_string_is_used_for_granted_path_in_serialized_response() {
        // Sanity-check that the wire-format conversion is in place.
        // The integration test `claim_files_accepts_string_form_files`
        // covers the live response shape; this is a static guard
        // against future regressions to `to_string_lossy`.
        let path = PathBuf::from("src/a.rs");
        let rendered = posix_string(&path);
        assert_eq!(rendered, "src/a.rs");
        assert!(!rendered.contains('\\'));
    }
}