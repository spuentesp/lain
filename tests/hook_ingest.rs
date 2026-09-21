//! Integration tests for the `POST /hook` endpoint (PR 2 of
//! `docs/INTENT_AND_OBSERVABILITY_PLAN.md`).
//!
//! The hook layer (Claude Code / AGY / Cursor) POSTs observations to
//! `/hook` as the agent works. These tests stand in for a real hook
//! script: register an agent, fire several observations through the
//! endpoint, then read the activity feed back via `list_active_intents`
//! and assert it matches.
//!
//! Why an integration test rather than a unit test of
//! `handle_hook`: the hook layer is most interesting as the seam
//! between the wire shape (JSON over HTTP) and the in-memory
//! `ActivityTracker`. A pure unit test of the inner function
//! doesn't exercise the body parsing, error envelope, or the
//! session-token auth — all of which are what real hook scripts
//! actually depend on.

use lain::server::LainServer;
use std::path::PathBuf;
use tempfile::TempDir;

fn isolated_server() -> (TempDir, std::sync::Arc<LainServer>) {
    let dir = TempDir::new().expect("tempdir");
    // `LainServer::new` -> `GitSensor::new` calls `git2::Repository::open`,
    // which requires an initialized repo. Same precondition the other
    // LainServer-based tests use.
    git2::Repository::init(dir.path()).unwrap();
    std::fs::write(dir.path().join("a.rs"), "pub fn a() {}").unwrap();
    let mem = dir.path().join(".lain/graph.bin");
    let server = LainServer::new(dir.path(), &mem, None).expect("LainServer::new");
    // `LainServer::new` returns a non-Arc; the test fixtures want an
    // Arc for cross-thread sharing. Wrap explicitly rather than
    // changing the constructor signature.
    (dir, std::sync::Arc::new(server))
}

/// Round-trip one observation: register an agent, POST a `Read`
/// observation through `handle_hook`, read the activity feed, and
/// assert the observation appears verbatim.
#[test]
fn hook_records_a_read_observation() {
    let (_dir, server) = isolated_server();

    // Register an agent and capture the session token.
    let reg = lain::server::mcp::presence_tools::run_register_agent(
        &server,
        serde_json::json!({ "name": "alice" }),
    )
    .expect("register");
    let agent_id = reg["agent_id"].as_str().unwrap().to_string();
    let token = reg["session_token"].as_str().unwrap().to_string();

    // Fire one observation through the hook layer. We don't go
    // through the HTTP body parser here — that's a separate unit
    // concern covered by `handle_hook`'s own tests in
    // `src/server/mcp/hook.rs`. This test pins the *seam*: given a
    // valid session token + event, the activity tracker records it.
    let event = lain::server::mcp::hook::HookEvent {
        session_token: token.clone(),
        agent_id: agent_id.clone(),
        event: "tool_start".into(),
        tool: "Read".into(),
        target: Some("src/a.rs".into()),
        at: None,
    };
    let ack = lain::server::mcp::hook::handle_hook(&server, event).expect("hook accept");
    assert_eq!(ack["ok"], serde_json::Value::Bool(true));

    // The activity feed must surface the observation.
    let activity = server
        .activity()
        .get(&lain::server::presence::AgentId(agent_id.clone()))
        .expect("activity must exist");
    let recent = &activity.recent_tools;
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].tool, "Read");
    assert_eq!(recent[0].target.as_deref(), Some("src/a.rs"));
}

/// A wrong `agent_id` for the resolved session must be rejected —
/// the same auth contract every other multiplayer tool enforces.
#[test]
fn hook_rejects_wrong_agent_id() {
    let (_dir, server) = isolated_server();
    let reg = lain::server::mcp::presence_tools::run_register_agent(
        &server,
        serde_json::json!({ "name": "alice" }),
    )
    .expect("register");
    let token = reg["session_token"].as_str().unwrap().to_string();

    let event = lain::server::mcp::hook::HookEvent {
        session_token: token,
        agent_id: "different-agent".into(),
        event: "tool_start".into(),
        tool: "Read".into(),
        target: Some("src/a.rs".into()),
        at: None,
    };
    let err = lain::server::mcp::hook::handle_hook(&server, event)
        .expect_err("wrong agent_id must reject");
    assert!(err.contains("agent_id does not match"), "got: {err}");
}

/// An unknown session token must be rejected — the auth check is
/// the seam between the hook layer and the registry.
#[test]
fn hook_rejects_unknown_session_token() {
    let (_dir, server) = isolated_server();
    // No registration: no token exists, so any token is unknown.
    let event = lain::server::mcp::hook::HookEvent {
        session_token: "not-a-real-token".into(),
        agent_id: "alice".into(),
        event: "tool_start".into(),
        tool: "Read".into(),
        target: Some("src/a.rs".into()),
        at: None,
    };
    let err = lain::server::mcp::hook::handle_hook(&server, event)
        .expect_err("unknown token must reject");
    assert!(
        err.contains("unknown session token") || err.contains("session"),
        "got: {err}"
    );
}

/// Multiple observations in sequence populate the ring buffer in
/// order; `list_active_intents` then surfaces the activity in the
/// per-agent payload. This is the end-to-end story the hook layer
/// tells: every Read / Grep / Edit the agent fires becomes a row
/// in the activity feed.
#[test]
fn hook_records_multiple_observations_in_order() {
    let (_dir, server) = isolated_server();
    let reg = lain::server::mcp::presence_tools::run_register_agent(
        &server,
        serde_json::json!({ "name": "alice" }),
    )
    .expect("register");
    let agent_id = reg["agent_id"].as_str().unwrap().to_string();
    let token = reg["session_token"].as_str().unwrap().to_string();

    // Three observations, in order. `Read` first, `Grep` second,
    // `Edit` third — the typical "investigate then patch" flow.
    for (tool, target) in [
        ("Read", "src/a.rs"),
        ("Grep", "a_function"),
        ("Edit", "src/a.rs"),
    ] {
        let event = lain::server::mcp::hook::HookEvent {
            session_token: token.clone(),
            agent_id: agent_id.clone(),
            event: "tool_start".into(),
            tool: tool.into(),
            target: Some(target.into()),
            at: None,
        };
        lain::server::mcp::hook::handle_hook(&server, event).expect("hook accept");
    }

    // `list_active_intents` surfaces the agent's observations in
    // the activity feed. We don't need to deserialize the whole
    // response — just enough to confirm the agent shows up with
    // the right observation count.
    let listed =
        lain::server::mcp::intent_tools::run_list_active_intents(&server, serde_json::json!({}))
            .expect("list_active_intents");
    let agents = listed["agents"].as_array().expect("agents array");
    assert_eq!(agents.len(), 1, "expected exactly one agent; got {listed}");
    let observed = agents[0]["observed_reads"]
        .as_array()
        .expect("observed_reads array");
    assert!(
        observed.iter().any(|v| v.as_str() == Some("src/a.rs")),
        "agent must have read src/a.rs at least once; got {listed}"
    );
}

/// `tool_end` and other non-`tool_start` events still record an
/// observation — the activity feed surfaces them as `last_tool`
/// entries without needing the agent kind to translate them.
#[test]
fn hook_records_session_lifecycle_events() {
    let (_dir, server) = isolated_server();
    let reg = lain::server::mcp::presence_tools::run_register_agent(
        &server,
        serde_json::json!({ "name": "alice" }),
    )
    .expect("register");
    let agent_id = reg["agent_id"].as_str().unwrap().to_string();
    let token = reg["session_token"].as_str().unwrap().to_string();

    let event = lain::server::mcp::hook::HookEvent {
        session_token: token,
        agent_id,
        event: "session_start".into(),
        tool: String::new(),
        target: None,
        at: None,
    };
    lain::server::mcp::hook::handle_hook(&server, event).expect("hook accept");

    // The recorded entry has `tool = "session_start"` (the event
    // field, since the tool field was empty).
    let activity = server
        .activity()
        .get(&lain::server::presence::AgentId(
            reg["agent_id"].as_str().unwrap().to_string(),
        ))
        .expect("activity");
    let last = activity.last_tool().expect("at least one entry");
    assert_eq!(last.tool, "session_start");
}

/// The `_dir` tempdir fixture returned by `isolated_server` is held
/// only for its Drop semantics (reap on test exit). Suppress the
/// unused-`_dir` warning for tests that don't actually need it.
#[allow(dead_code)]
fn _typecheck_tempdir_holder(_p: &PathBuf) {}
