//! Hook ingestion endpoint (PR 2 of
//! `docs/INTENT_AND_OBSERVABILITY_PLAN.md`).
//!
//! Agents fire tool calls through their host (Claude Code, Cursor,
//! Codex, AGY, …). Each host's hook layer wraps every tool call
//! and POSTs the observation to `/hook`. This file turns that wire
//! shape into a single `ActivityTracker::record` call, gated by the
//! same session-token auth every other multiplayer tool uses.
//!
//! Wire shape (the schema the hook script sends):
//! ```json
//! {
//!   "session_token": "<token>",
//!   "agent_id": "<id>",
//!   "event": "tool_start" | "tool_end" | ...,
//!   "tool": "Read" | "Edit" | ...,
//!   "target": "src/auth.rs",
//!   "at": "2026-09-20T17:42:01Z"   // optional; defaults to server now()
//! }
//! ```
//!
//! Why a free function (`handle_hook`) rather than a method on
//! `LainServer`: the same reason `save_pair` is a free function in
//! `presence.rs` — keeps the HTTP handler's signature flat and lets
//! the hook logic stay close to the wire shape it validates.

use crate::server::mcp::presence_tools::authenticate;
use crate::server::LainServer;
use serde::Deserialize;
use serde_json::Value;

/// Wire shape for one `/hook` POST. Every field except `at` is
/// required; serde rejects the request as `malformed_hook_event` if
/// `session_token` / `agent_id` / `event` / `tool` are missing.
#[derive(Clone, Debug, Deserialize)]
pub struct HookEvent {
    pub session_token: String,
    pub agent_id: String,
    /// One of `tool_start`, `tool_end`, `session_start`,
    /// `session_end`, `user_prompt`, `subagent_start`,
    /// `subagent_end`. The hook layer only records `tool_start` /
    /// `tool_end` into the activity ring buffer; the other events
    /// are accepted for forward compatibility and (in PR 3+)
    /// trigger pre-edit evaluation or intent nudges.
    pub event: String,
    /// Tool name as the agent-kind reports it (e.g. `"Read"`,
    /// `"Edit"`). Stored verbatim in `ObservedTool`.
    pub tool: String,
    /// Best-effort target. `None` for tools that have no obvious
    /// target (e.g. agent-internal operations). The serde shape
    /// allows it to be omitted.
    #[serde(default)]
    pub target: Option<String>,
    /// Optional ISO-8601 timestamp. When absent, the server stamps
    /// `SystemTime::now()` at record time.
    #[serde(default)]
    pub at: Option<String>,
}

/// Authenticate the session token and record the observation in the
/// activity tracker. The returned `Value` is the JSON success body
/// the hook sees; failures return `Err(String)` so the HTTP layer
/// can produce a 400 response with a stable error code.
pub fn handle_hook(server: &LainServer, event: HookEvent) -> Result<Value, String> {
    // Wrap the activity mutation in `with_shared_presence` so the
    // observation write to the state file happens under the lock.
    // Without this wrapper the activity tracker's persist callback
    // fires outside the lock and races against `claim_files`
    // writes on the same state file — the natural-contention race
    // the user's report identified. The HookEvent is Clone so the
    // closure can be FnOnce.
    server
        .with_shared_presence(|| handle_hook_inner(server, event.clone()))
        .map_err(|e| e.to_string())
        .and_then(|inner| inner)
}

fn handle_hook_inner(server: &LainServer, event: HookEvent) -> Result<Value, String> {
    // Auth is the standard session-token check. The agent_id must
    // match the resolved session — same contract every other
    // multiplayer tool enforces — so a hook forger can't record
    // observations under another agent's name.
    let session = authenticate(server, &event.session_token)?;
    if session.id.as_str() != event.agent_id {
        return Err("agent_id does not match session token".into());
    }

    // The activity tracker records every event regardless of type;
    // `ObservedTool.target` already holds the tool name so we use
    // `event` as the `tool` field for non-tool events when no
    // explicit tool is given. This way the activity feed surfaces
    // "session_start" / "session_end" etc. in `last_tool` without a
    // separate field.
    let tool = if event.tool.is_empty() {
        event.event.clone()
    } else {
        event.tool.clone()
    };

    server
        .activity()
        .record(session.id.clone(), tool, event.target.clone());

    Ok(serde_json::json!({
        "ok": true,
        "agent_id": session.id.as_str(),
        "event": event.event,
        "tool": event.tool,
        "target": event.target,
    }))
}

/// Synchronous pre-edit consultation. The agent calls this just
/// before mutating a file; the server returns the same
/// `CoordinationLevel` that the baseline `lain_intent` would return,
/// but synchronously and scoped to the specific target the agent is
/// about to edit. This is the missing piece of the agent UX: the
/// baseline evaluation runs on every `lain_intent` call, but an
/// agent that wants to consult before a specific Edit no longer has
/// to issue a full intent declaration first.
///
/// Wire shape (the request body):
/// ```json
/// {
///   "session_token": "...",
///   "agent_id": "...",
///   "target": "src/auth.rs",
///   "intent": "edit" | "read"
/// }
/// ```
///
/// Response (the success body):
/// ```json
/// {
///   "level": "green|yellow|red",
///   "reason": { ... } | null,
///   "related": [ ... ]
/// }
/// ```
///
/// Returns `Err(String)` for any failure so the HTTP layer can
/// produce a 4xx response with a stable error code. Errors are
/// stable so the agent can branch on them: `auth_*` codes are 401,
/// `invalid_*` codes are 400, anything else is 400 by default.
pub fn evaluate(server: &LainServer, body: Value) -> Result<Value, String> {
    // Auth + identity check, same contract every other multiplayer
    // tool enforces. session_token must resolve; the agent_id in
    // the body must match the resolved session.
    let session_token = body
        .get("session_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing session_token".to_string())?;
    let agent_id = body
        .get("agent_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing agent_id".to_string())?;
    let target = body
        .get("target")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing target".to_string())?;
    let intent_str = body
        .get("intent")
        .and_then(|v| v.as_str())
        .unwrap_or("edit");

    let session = authenticate(server, session_token)?;
    if session.id.as_str() != agent_id {
        return Err("auth_mismatch: agent_id does not match session token".into());
    }

    // Pull the agent's current intent (if any) so we know its scope
    // declarations. This mirrors `lain_intent`'s baseline evaluation
    // but doesn't require a separate `add_scopes` call.
    let agent_intent = server.intent().get(&session.id);
    let scopes: Vec<String> = match &agent_intent {
        Some(i) => i.scopes.clone(),
        None => Vec::new(),
    };

    // Build an EvalContext and run the existing evaluator. Reuse
    // the same code path that `lain_intent` uses — that way a fix
    // to the rules (e.g. the planned graph-distance refinement)
    // automatically applies here too.
    let intent = crate::server::intent::Intent {
        id: agent_intent
            .as_ref()
            .map(|i| i.id.clone())
            .unwrap_or_default(),
        agent_id: session.id.clone(),
        goal: agent_intent
            .as_ref()
            .map(|i| i.goal.clone())
            .unwrap_or_default(),
        scopes: scopes.clone(),
        status: agent_intent.as_ref().map(|i| i.status).unwrap_or_default(),
        created_at: std::time::SystemTime::now(),
        updated_at: std::time::SystemTime::now(),
    };
    let peer_intents: Vec<crate::server::intent::Intent> = server
        .intent()
        .list_active()
        .into_iter()
        .filter(|i| i.agent_id != session.id)
        .collect();
    let peer_intents_refs: Vec<&crate::server::intent::Intent> = peer_intents.iter().collect();
    let peer_claims: Vec<crate::server::presence::Claim> = server
        .occupancy()
        .list_all()
        .into_iter()
        .flat_map(|entry| {
            entry
                .agents
                .into_iter()
                .filter(|aid| aid != &session.id)
                .map(move |aid| (aid, entry.path.clone(), entry.symbols.clone()))
        })
        .filter_map(|(aid, path, _symbols)| {
            let claims = server.occupancy().list_for_agent(&aid);
            claims.into_iter().find(|c| c.path == path).map(|c| {
                let mut claim = c;
                claim.agent_id = aid;
                claim
            })
        })
        .collect();
    let peer_claims_refs: Vec<&crate::server::presence::Claim> = peer_claims.iter().collect();
    let ctx = crate::server::evaluation::EvalContext {
        intent: if scopes.is_empty() {
            None
        } else {
            Some(&intent)
        },
        target,
        peer_intents: peer_intents_refs,
        peer_claims: peer_claims_refs,
        peer_reading: std::collections::HashSet::new(),
        // No graph reference passed here — `evaluate()` falls back
        // to lexical `path_distance` for symbol-scope entries that
        // look like `module::symbol`. PR 3's graph refinement is
        // only active in `lain_intent`'s baseline where the graph is
        // available.
        graph: None,
    };
    let _ = intent_str; // accepted as documentation; the evaluator
                        // doesn't branch on read vs edit (read claims
                        // never block; RED is the only gate).
    let level = crate::server::evaluation::evaluate(ctx);
    serde_json::to_value(&level).map_err(|e| format!("serialize: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire shape is permissive: `target` and `at` are optional.
    /// A missing `target` must deserialize to `None` rather than
    /// erroring, because tool calls like `Bash` (no obvious file
    /// target) come through without one.
    #[test]
    fn hook_event_parses_minimal_shape() {
        let v: HookEvent = serde_json::from_value(serde_json::json!({
            "session_token": "tok",
            "agent_id": "a",
            "event": "tool_start",
            "tool": "Read",
        }))
        .expect("minimal shape must parse");
        assert_eq!(v.session_token, "tok");
        assert_eq!(v.agent_id, "a");
        assert_eq!(v.event, "tool_start");
        assert_eq!(v.tool, "Read");
        assert!(v.target.is_none());
        assert!(v.at.is_none());
    }

    /// Missing required fields surface as serde parse errors — the
    /// HTTP layer reports them as `malformed_hook_event`. This pins
    /// the contract so a hook script that forgets `session_token`
    /// gets a clear 400, not a 500 from a downstream unwrap.
    #[test]
    fn hook_event_missing_required_field_fails_to_parse() {
        let res: Result<HookEvent, _> = serde_json::from_value(serde_json::json!({
            "agent_id": "a",
            "event": "tool_start",
            "tool": "Read",
        }));
        assert!(
            res.is_err(),
            "missing session_token must surface as a serde error; got Ok"
        );
    }

    /// `evaluate` validates the body shape up front and returns
    /// stable error codes so the HTTP layer can map them to 4xx
    /// responses without parsing free-form strings.
    #[test]
    fn evaluate_returns_missing_session_token() {
        let (_dir, server) = isolated_server();
        let res = evaluate(
            &server,
            serde_json::json!({
                "agent_id": "a",
                "target": "src/a.rs",
            }),
        );
        assert!(res.is_err());
        assert!(
            res.unwrap_err().contains("session_token"),
            "error must mention session_token"
        );
    }

    #[test]
    fn evaluate_returns_missing_agent_id() {
        let (_dir, server) = isolated_server();
        let res = evaluate(
            &server,
            serde_json::json!({
                "session_token": "tok",
                "target": "src/a.rs",
            }),
        );
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("agent_id"));
    }

    #[test]
    fn evaluate_returns_missing_target() {
        let (_dir, server) = isolated_server();
        let res = evaluate(
            &server,
            serde_json::json!({
                "session_token": "tok",
                "agent_id": "a",
            }),
        );
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("target"));
    }

    #[test]
    fn evaluate_returns_auth_mismatch_when_agent_id_differs() {
        let (_dir, server) = isolated_server();
        let (aid, tok) = register_agent(&server, "alice");
        let res = evaluate(
            &server,
            serde_json::json!({
                "session_token": tok,
                "agent_id": aid,
                "target": "src/a.rs",
            }),
        );
        assert!(res.is_ok(), "same agent_id should pass auth");
        let bad = evaluate(
            &server,
            serde_json::json!({
                "session_token": tok,
                "agent_id": "different-agent-id",
                "target": "src/a.rs",
            }),
        );
        assert!(bad.is_err());
        assert!(bad.unwrap_err().starts_with("auth_mismatch"));
    }

    /// Happy path: registered agent with declared scopes evaluates
    /// to a GREEN level when no peer is in scope.
    #[test]
    fn evaluate_returns_green_when_no_peer_overlap() {
        let (_dir, server) = isolated_server();
        let (aid, tok) = register_agent(&server, "alice");
        server.intent().declare(
            crate::server::presence::AgentId(aid.clone()),
            "refactor auth".into(),
            vec!["src/auth.rs".into()],
            crate::server::intent::IntentStatus::Editing,
        );
        let res = evaluate(
            &server,
            serde_json::json!({
                "session_token": tok,
                "agent_id": aid,
                "target": "src/auth.rs",
            }),
        )
        .expect("evaluate should succeed");
        assert_eq!(res["level"], "green");
    }

    /// Build a minimal LainServer with the multiplayer surface
    /// wired up so `evaluate` can call `server.intent()` etc.
    /// without panicking. Adapted from `tests/use_cases/...` fixture
    /// helpers in this repo.
    fn isolated_server() -> (tempfile::TempDir, std::sync::Arc<crate::server::LainServer>) {
        use crate::server::ingest::LainServer;
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_path_buf();
        std::fs::create_dir_all(ws.join(".lain")).unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&ws)
            .output()
            .ok();
        let mem = ws.join(".lain/graph.bin");
        let server = LainServer::new(&ws, &mem, None).expect("LainServer::new");
        (dir, std::sync::Arc::new(server))
    }

    /// Helper: register one agent, return (agent_id, token).
    fn register_agent(
        server: &std::sync::Arc<crate::server::LainServer>,
        name: &str,
    ) -> (String, String) {
        let session = server.presence().register(
            name.to_string(),
            crate::server::presence::AgentKind::ClaudeCode,
            crate::server::presence::AgentMode::Interactive,
            None,
            None,
        );
        (session.id.as_str().to_string(), session.session_token)
    }
}
