//! MCP tools for the intent + activity feed surface.
//!
//! PR 1 of `docs/INTENT_AND_OBSERVABILITY_PLAN.md` introduces two new
//! MCP tools: `lain_intent` (declare or update an intent) and
//! `list_active_intents` (return the per-agent activity feed).
//! Future PRs layer the hook ingestion (`POST /hook`) and the
//! pre-edit evaluator (`GREEN / YELLOW / RED`) on top.
//!
//! ## Why a separate file
//!
//! The 8 multiplayer tools (`register_agent`, `heartbeat`,
//! `list_active_agents`, `who_am_i`, `list_subagents`, `claim_files`,
//! `release_files`, `my_claims`) live in `presence_tools.rs` and
//! are dispatched via the inventory pattern (`declare_presence_tool!`
//! macros in `handler.rs`). The intent layer is a sibling surface —
//! the dispatch lives at the same `mcp/handler.rs` site but the
//! handlers are split out into this file so the existing presence
//! module stays focused. New tools added to the intent layer go in
//! this file and register with the same macro machinery.

use crate::server::intent::{Intent, IntentRegistry, IntentStatus};
use crate::server::mcp::presence_tools::authenticate;
use crate::server::presence::{AgentId, AgentSession};
use crate::server::LainServer;
use serde::Deserialize;
use serde_json::{json, Value};

/// `lain_intent` request body. The same shape covers both declare
/// (`intent_id` absent) and update (`intent_id` present) — serde
/// accepts either; the inner logic branches on whether
/// `intent_id` is `None`.
///
/// Field semantics:
/// - `goal` (declare / update): free-form text describing the
///   agent's objective. Not parsed; rendered as-is.
/// - `scopes` (declare): the initial list of symbol or path scopes.
///   On update, this is ignored — use `add_scopes` / `remove_scopes`.
/// - `add_scopes` / `remove_scopes` (update): partial-update
///   fields. `add_scopes` appends without duplicates; `remove_scopes`
///   filters out by exact match.
/// - `status` (declare / update): one of `investigating`, `planning`,
///   `editing`, `reviewing`, `done`. Defaults to `planning` on declare
///   when absent.
#[derive(Clone, Debug, Deserialize)]
pub struct LainIntentArgs {
    pub agent_id: String,
    pub session_token: String,
    #[serde(default)]
    pub intent_id: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    #[serde(default)]
    pub add_scopes: Option<Vec<String>>,
    #[serde(default)]
    pub remove_scopes: Option<Vec<String>>,
    #[serde(default)]
    pub status: Option<String>,
}

/// Declare or update an intent. The branch is implicit:
/// - `intent_id` absent → declare. `goal` is required (else the
///   intent has nothing to declare); `scopes` is optional.
/// - `intent_id` present → update. `goal` / `status` / `add_scopes` /
///   `remove_scopes` are optional and partial-update.
///
/// Auth is the same as every other multiplayer tool: the caller
/// must present a session token that resolves to the same agent id.
///
/// The response shape includes the new `intent_id`, the current
/// `revision` (an overlay revision counter — populated with `0` for
/// now; PR 3's pre-edit evaluator will populate it from the live
/// overlay), and a placeholder `coordination` block. PR 3 wires the
/// actual GREEN/YELLOW/RED evaluation here.
pub fn run_lain_intent(server: &LainServer, args: Value) -> Result<Value, String> {
    // Wrap the entire declare/update + coordination evaluation in
    // `with_shared_presence` so the intent mutation and its persist
    // happen under the state-file lock. Without this wrapper,
    // `intent_registry.declare()` / `update()` fire the persist
    // callback *outside* the lock and race against `claim_files`
    // writes on the same state file. That's the natural-contention
    // race the user's report identified.
    server
        .with_shared_presence(|| run_lain_intent_inner(server, args.clone()))
        .map_err(|e| e.to_string())
        .and_then(|inner| inner)
}

fn run_lain_intent_inner(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: LainIntentArgs =
        serde_json::from_value(args).map_err(|e| format!("lain_intent: {e}"))?;
    let session = authenticate(server, &a.session_token)?;
    if session.id.as_str() != a.agent_id {
        return Err("agent_id does not match session token".into());
    }
    let agent_id = session.id.clone();
    let intent_registry: &IntentRegistry = server.intent();

    let intent = match a.intent_id {
        None => {
            // Declare path.
            let goal = a
                .goal
                .ok_or_else(|| "lain_intent: goal is required when declaring".to_string())?;
            let scopes = a.scopes.unwrap_or_default();
            let status = a
                .status
                .as_deref()
                .map(IntentStatus::parse)
                .unwrap_or_default();
            let _id = intent_registry.declare(agent_id, goal, scopes, status);
            // Re-read the just-declared intent so the response shape
            // is uniform across declare and update.
            intent_registry
                .get(&session.id)
                .ok_or_else(|| "lain_intent: declared intent disappeared".to_string())?
        }
        Some(intent_id_str) => {
            // Update path. Empty strings / `None` mean "leave alone".
            let intent_id = crate::server::intent::IntentId(intent_id_str);
            let add_scopes = a.add_scopes.unwrap_or_default();
            let remove_scopes = a.remove_scopes.unwrap_or_default();
            intent_registry.update(
                session.id.clone(),
                intent_id,
                add_scopes,
                remove_scopes,
                a.goal,
                a.status.as_deref().map(IntentStatus::parse),
            )?
        }
    };

    // Baseline coordination evaluation (PR 3): without a target
    // argument, the agent is asking "is the workspace ready for
    // me to start working on the scope I just declared?". The
    // evaluator checks peer intent overlap against the agent's
    // own scopes — same logic the pre-edit hook will use, just
    // without a target. The first scope (or a placeholder when
    // none) stands in as the target so the level reflects peer
    // activity on the agent's own surface.
    let all_intents = intent_registry.list_active();
    let peer_intents: Vec<&Intent> = all_intents
        .iter()
        .filter(|i| i.agent_id != intent.agent_id)
        .collect();
    // Pull peer claims by walking `OccupancyMap::list_for_agent` for
    // each peer (the `OccupancyMap` API exposes a per-agent listing;
    // there is no flat "all peer claims" surface, and `list_all`
    // strips the per-claim `intent` field which the evaluator
    // needs). One list_for_agent call per peer is fine — the agent
    // count is bounded by what the workspace can sustain.
    let peer_agent_ids: Vec<crate::server::presence::AgentId> =
        peer_intents.iter().map(|i| i.agent_id.clone()).collect();
    let mut peer_claims: Vec<crate::server::presence::Claim> = Vec::new();
    for aid in &peer_agent_ids {
        for mut c in server.occupancy().list_for_agent(aid) {
            // Defensive: ensure the claim's `agent_id` field matches
            // the agent id we asked about. The occupancy map owns
            // both ends so this should always be true, but a future
            // refactor that splits claim storage from agent
            // ownership could drift.
            if c.agent_id != *aid {
                c.agent_id = aid.clone();
            }
            peer_claims.push(c);
        }
    }
    let baseline_target: &str = intent.scopes.first().map(String::as_str).unwrap_or("");
    // Bind the owned vec so the references below outlive the eval.
    let peer_claims_refs: Vec<&crate::server::presence::Claim> = peer_claims.iter().collect();
    // Degenerate intent (no scopes) — there's no target to evaluate
    // against, so the baseline can't be YELLOW for "outside scope".
    // GREEN is the honest answer: we don't know what the agent
    // intends to edit, so we have nothing to flag. The agent can
    // update the intent with `add_scopes` once it has a target.
    let coordination = if intent.scopes.is_empty() {
        crate::server::evaluation::CoordinationLevel::Green {
            related: Vec::new(),
        }
    } else {
        let ctx = crate::server::evaluation::EvalContext {
            intent: Some(&intent),
            target: baseline_target,
            peer_intents,
            peer_claims: peer_claims_refs,
            // PR 3 doesn't plumb peer reading yet — that's wired
            // when the pre-edit hook endpoint lands.
            peer_reading: std::collections::HashSet::new(),
            // Graph-distance refinement: when a static graph is
            // available (single-repo + federation modes), BFS
            // over `Calls` edges surfaces symbol-scope overlap
            // that lexical `path_distance` would miss. The BFS
            // is bounded to 32 hops; see `evaluate()` for the
            // fallback to lexical distance.
            graph: Some(server.ingest().graph()),
        };
        crate::server::evaluation::evaluate(ctx)
    };

    Ok(intent_to_response_full(
        &intent,
        &coordination,
        server.overlay().current_revision(),
    ))
}

/// Build the wire response for `lain_intent` (PR 3). The
/// `coordination` field carries the live `CoordinationLevel` from
/// the evaluation engine, so the agent sees the same level the
/// pre-edit hook will use on its behalf.
fn intent_to_response_full(
    intent: &Intent,
    coordination: &crate::server::evaluation::CoordinationLevel,
    revision: u64,
) -> Value {
    let coordination_value =
        serde_json::to_value(coordination).expect("CoordinationLevel serializes to JSON");
    json!({
        "intent_id": intent.id.as_str(),
        "revision": revision,
        "coordination": coordination_value,
        "intent": {
            "agent_id": intent.agent_id.as_str(),
            "goal": intent.goal,
            "scopes": intent.scopes,
            "status": intent.status.as_str(),
            "created_at_unix": crate::server::time::unix_secs_u64(intent.created_at),
            "updated_at_unix": crate::server::time::unix_secs_u64(intent.updated_at),
        }
    })
}

#[derive(Debug, Deserialize)]
pub struct ListActiveIntentsArgs {
    /// When `Some`, only include this agent's entry. Useful for a
    /// polling dashboard that wants one agent's row.
    #[serde(default)]
    pub agent_id: Option<String>,
}

/// `list_active_intents` returns the per-agent activity model — the
/// same shape `who_am_i` / `list_active_agents` will surface in PR 1's
/// last step (the `extend who_am_i / list_active_agents payloads`
/// item). The MCP tool exists separately so a peer agent can pull
/// the activity feed without an `register_agent` round-trip first,
/// and so a dashboard can fetch every agent at once.
pub fn run_list_active_intents(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: ListActiveIntentsArgs =
        serde_json::from_value(args).unwrap_or(ListActiveIntentsArgs { agent_id: None });

    // Build a unified per-agent view. Each entry joins the agent's
    // intent (if any) with their activity feed (if any). The
    // session-token check is intentionally NOT performed: this is a
    // read-only observation, and the activity feed is exactly the
    // thing a peer agent needs to see without authenticating as
    // another agent.
    let intent_registry = server.intent();
    let activity_tracker = server.activity();

    let intents = intent_registry.list_active();
    // `snapshot` is `(agent_id, Intent)`; convert to a lookup map.
    // The registry enforces "one intent per agent", so a HashMap is
    // safe.
    let intent_by_agent: std::collections::HashMap<AgentId, Intent> = intents
        .into_iter()
        .map(|i| (i.agent_id.clone(), i))
        .collect();

    let activity_entries = activity_tracker.snapshot();

    let mut agents: Vec<Value> = Vec::new();

    // First, agents with an intent (they're the primary surface).
    for (agent_id, intent) in &intent_by_agent {
        let activity = activity_tracker.get(agent_id);
        agents.push(per_agent_view(agent_id, Some(intent), activity.as_ref()));
    }

    // Then agents with activity but no declared intent — the
    // observation is still useful (you can see what files they've
    // read) even without a goal text.
    if a.agent_id.is_none() {
        for (agent_id, activity) in &activity_entries {
            if intent_by_agent.contains_key(agent_id) {
                continue;
            }
            agents.push(per_agent_view(agent_id, None, Some(activity)));
        }
    }

    // Sort by `updated_at_unix` so the freshest row surfaces first.
    // Agents without an intent (so no `updated_at_unix`) sort last;
    // the dashboard can filter or ignore them.
    agents.sort_by(|a, b| {
        let a_ts = a
            .get("intent")
            .and_then(|i| i.get("updated_at_unix"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let b_ts = b
            .get("intent")
            .and_then(|i| i.get("updated_at_unix"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        b_ts.cmp(&a_ts)
    });

    Ok(json!({ "agents": agents }))
}

fn per_agent_view(
    agent_id: &AgentId,
    intent: Option<&Intent>,
    activity: Option<&crate::server::activity::Activity>,
) -> Value {
    // The wire shape is the same one `who_am_i` /
    // `list_active_agents` will extend to carry — single source of
    // truth for the per-agent payload. Each branch is hoisted into
    // its own `let` so the macro parser doesn't get tangled on
    // nested braces inside `Option::map`.
    let intent_json = intent.map(|i| {
        json!({
            "agent_id": i.agent_id.as_str(),
            "goal": i.goal,
            "scopes": i.scopes,
            "status": i.status.as_str(),
            "created_at_unix": crate::server::time::unix_secs_u64(i.created_at),
            "updated_at_unix": crate::server::time::unix_secs_u64(i.updated_at),
        })
    });
    let observed_reads = activity.map(|a| a.observed_reads()).unwrap_or_default();
    let focus = activity.map(|a| a.focus()).unwrap_or_default();
    let last_tool_json = activity.and_then(|a| a.last_tool().cloned()).map(|t| {
        json!({
            "tool": t.tool,
            "target": t.target,
            "at_unix": crate::server::time::unix_secs_u64(t.at),
        })
    });

    json!({
        "agent_id": agent_id.as_str(),
        "intent": intent_json,
        "focus": focus,
        "observed_reads": observed_reads,
        "last_tool": last_tool_json,
    })
}

// `authenticate` lives in `presence_tools` — re-export here so a
// future caller can import it from this module without reaching
// into the sibling one. The agent-id equality check (above) is the
// standard multiplayer contract.
#[allow(dead_code)]
fn _authenticate_signature_check(server: &LainServer, token: &str) -> Result<AgentSession, String> {
    authenticate(server, token)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure serde round-trip for the request shape. Asserts that
    /// every documented field is parsed (the `#[serde(default)]`
    /// annotations on optional fields matter: an agent that omits
    /// `goal` on declare should get a clear "goal is required" error
    /// rather than a serde parse failure).
    #[test]
    fn lain_intent_args_parses_minimal_declare() {
        let v: LainIntentArgs = serde_json::from_value(serde_json::json!({
            "agent_id": "a",
            "session_token": "tok",
            "goal": "refactor auth",
            "scopes": ["auth::validate_token"],
            "status": "editing",
        }))
        .expect("declare shape must parse");
        assert_eq!(v.agent_id, "a");
        assert_eq!(v.session_token, "tok");
        assert!(v.intent_id.is_none());
        assert_eq!(v.goal.as_deref(), Some("refactor auth"));
        assert_eq!(
            v.scopes.as_ref().unwrap(),
            &vec!["auth::validate_token".to_string()]
        );
        assert_eq!(v.status.as_deref(), Some("editing"));
        assert!(v.add_scopes.is_none());
        assert!(v.remove_scopes.is_none());
    }

    #[test]
    fn lain_intent_args_parses_update_with_partial_fields() {
        let v: LainIntentArgs = serde_json::from_value(serde_json::json!({
            "agent_id": "a",
            "session_token": "tok",
            "intent_id": "I-184",
            "add_scopes": ["session::SessionClaims"],
            "status": "reviewing",
        }))
        .expect("update shape must parse");
        assert_eq!(v.intent_id.as_deref(), Some("I-184"));
        assert!(v.goal.is_none(), "update with no goal leaves goal None");
        assert!(
            v.scopes.is_none(),
            "scopes on update path is ignored — must be None"
        );
        assert_eq!(
            v.add_scopes.as_ref().unwrap(),
            &vec!["session::SessionClaims".to_string()]
        );
        assert!(v.remove_scopes.is_none());
    }

    /// List-args shape: filter is optional, defaults to "all".
    #[test]
    fn list_active_intents_args_default_is_all_agents() {
        let v: ListActiveIntentsArgs = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(v.agent_id.is_none());
    }
}
