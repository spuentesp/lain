//! Two servers, one workspace: presence must be shared (F-02).
//!
//! Presence lived only in each process's memory, and the MCP stdio
//! transport spawns one server *per client*. Two Claude Code windows on
//! the same repo therefore ran two registries and could not see each
//! other: every claim was granted, no conflict was ever reported, and
//! nothing indicated the coordination layer was inert. The README's
//! "Wire your agent" section configures stdio, so the default install
//! produced exactly that topology.
//!
//! These tests stand in for two processes by building two independent
//! `LainServer` values over the same workspace — which is the same
//! thing from the state file's point of view.

use lain::server::mcp::presence_tools::{
    run_claim_files, run_list_active_agents, run_register_agent, run_release_files,
};
use lain::server::LainServer;
use std::sync::Arc;

/// `state_dir()` resolves through `XDG_STATE_HOME`; point it at a
/// tempdir so tests never touch the developer's real state.
/// Process-wide, so this file's tests are serialized behind one mutex.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn isolate_state_dir(tmp: &std::path::Path) {
    std::env::set_var("XDG_STATE_HOME", tmp.to_string_lossy().to_string());
}

fn server_for(workspace: &std::path::Path) -> Arc<LainServer> {
    let mem = workspace.join(".lain/graph.bin");
    Arc::new(LainServer::new(workspace, &mem, None).expect("LainServer::new"))
}

fn register(server: &Arc<LainServer>, name: &str) -> (String, String) {
    let v = run_register_agent(server, serde_json::json!({ "name": name })).unwrap();
    (
        v["agent_id"].as_str().unwrap().to_string(),
        v["session_token"].as_str().unwrap().to_string(),
    )
}

fn claim(server: &Arc<LainServer>, id: &str, token: &str, path: &str) -> serde_json::Value {
    run_claim_files(
        server,
        serde_json::json!({
            "agent_id": id,
            "session_token": token,
            "files": [{ "path": path, "intent": "edit" }],
        }),
    )
    .unwrap()
}

#[tokio::test]
async fn a_claim_on_one_server_conflicts_on_another() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let state = tempfile::tempdir().unwrap();
    isolate_state_dir(state.path());

    let ws = tempfile::tempdir().unwrap();
    git2::Repository::init(ws.path()).unwrap();
    std::fs::create_dir_all(ws.path().join("src")).unwrap();
    std::fs::write(ws.path().join("src/handler.rs"), "pub fn a() {}").unwrap();

    // Two servers over one workspace — two Claude Code windows.
    let alice_server = server_for(ws.path());
    let bob_server = server_for(ws.path());

    let (alice, alice_token) = register(&alice_server, "alice");
    let (bob, bob_token) = register(&bob_server, "bob");

    let granted = claim(&alice_server, &alice, &alice_token, "src/handler.rs");
    assert_eq!(
        granted["granted"].as_array().unwrap().len(),
        1,
        "alice should hold the file; resp={granted}"
    );

    // Bob is on a different server. Before the state file was shared,
    // this returned `{"conflicts": [], "granted": [...]}` — both agents
    // editing the same file, neither aware of the other.
    let resp = claim(&bob_server, &bob, &bob_token, "src/handler.rs");
    let conflicts = resp["conflicts"].as_array().unwrap();
    assert_eq!(
        conflicts.len(),
        1,
        "bob must see alice's claim from the other server; resp={resp}"
    );
    assert_eq!(conflicts[0]["agent_id"].as_str().unwrap(), alice);
    assert_eq!(
        resp["granted"].as_array().unwrap().len(),
        0,
        "a conflicting claim must not be granted; resp={resp}"
    );
}

#[tokio::test]
async fn agents_registered_on_one_server_are_visible_on_another() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let state = tempfile::tempdir().unwrap();
    isolate_state_dir(state.path());

    let ws = tempfile::tempdir().unwrap();
    git2::Repository::init(ws.path()).unwrap();

    let first = server_for(ws.path());
    let second = server_for(ws.path());

    let (alice, _) = register(&first, "alice");

    let listed = run_list_active_agents(&second, serde_json::json!({})).unwrap();
    let names: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["agent_id"].as_str())
        .collect();
    assert!(
        names.contains(&alice.as_str()),
        "an agent registered on one server must be visible on another; got {listed}"
    );
}

#[tokio::test]
async fn releasing_on_one_server_frees_the_file_on_another() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let state = tempfile::tempdir().unwrap();
    isolate_state_dir(state.path());

    let ws = tempfile::tempdir().unwrap();
    git2::Repository::init(ws.path()).unwrap();
    std::fs::create_dir_all(ws.path().join("src")).unwrap();
    std::fs::write(ws.path().join("src/handler.rs"), "pub fn a() {}").unwrap();

    let alice_server = server_for(ws.path());
    let bob_server = server_for(ws.path());
    let (alice, alice_token) = register(&alice_server, "alice");
    let (bob, bob_token) = register(&bob_server, "bob");

    claim(&alice_server, &alice, &alice_token, "src/handler.rs");
    run_release_files(
        &alice_server,
        serde_json::json!({
            "agent_id": alice,
            "session_token": alice_token,
            "files": [{ "path": "src/handler.rs" }],
        }),
    )
    .unwrap();

    // Bob's server must observe the release, not just the claim.
    let resp = claim(&bob_server, &bob, &bob_token, "src/handler.rs");
    assert_eq!(
        resp["conflicts"].as_array().unwrap().len(),
        0,
        "a released file must be free on every server; resp={resp}"
    );
    assert_eq!(resp["granted"].as_array().unwrap().len(), 1);
}
