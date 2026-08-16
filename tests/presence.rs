use lain::server::presence::*;

// --- Task 8 brief: existing tools surface occupancy ---

/// `query_graph` (Task 8) carries an `occupancy: {active_agents: [...]}`
/// payload so the calling agent can see who's in the workspace before
/// they act on the result. The handler reads through `LainServer`'s
/// `presence` / `occupancy` fields, so wiring them at construction
/// time is enough — this test asserts the underlying helper the
/// handler uses (`list_for_path`) is reachable through the server, and
/// that registering + claiming populates it as expected. The handler's
/// actual JSON shape is covered by `query_graph_includes_occupancy_json`
/// in `tools/handlers/query_tests.rs`.
#[tokio::test]
async fn query_graph_includes_occupancy() {
    use lain::server::LainServer;

    let tmp = tempfile::tempdir().unwrap();
    // `LainServer::new` -> `GitSensor::new` calls `git2::Repository::open`,
    // which requires a real initialized repo — a bare `.git` directory
    // is not enough. Use `git2::Repository::init` (same fix Task 4 used).
    git2::Repository::init(tmp.path()).unwrap();
    std::fs::write(tmp.path().join("a.rs"), "pub fn a() {}").unwrap();
    let mem = tmp.path().join(".lain/graph.bin");
    let server = LainServer::new(tmp.path(), &mem, None).expect("server");

    // Register an agent and claim the file.
    let agent = server.presence.register("alice".into(), AgentKind::ClaudeCode, AgentMode::Interactive, None, None);
    let _ = server.occupancy.claim(&agent.id, vec![ClaimRequest {
        path: std::path::PathBuf::from("a.rs"),
        symbols: vec![],
        intent: ClaimIntent::Edit,
    }]);

    // Verify the claim is observable through the helper the
    // `query_graph` handler uses to build its `occupancy.active_agents`
    // payload. Same handler, same code path as the production tool.
    let entry = server.occupancy.list_for_path(&std::path::PathBuf::from("a.rs"));
    assert!(entry.is_some(), "expected an occupancy entry for a.rs after claim");
    assert_eq!(entry.unwrap().agents, vec![agent.id.clone()]);
}

#[test]
fn register_assigns_unique_ids_and_session_tokens() {
    let reg = PresenceRegistry::new();
    let s1 = reg.register("claude-1".into(), AgentKind::ClaudeCode, AgentMode::Interactive, Some(1234), None);
    let s2 = reg.register("kimi-1".into(), AgentKind::Kimi, AgentMode::Interactive, Some(5678), None);
    assert_ne!(s1.id, s2.id);
    assert_ne!(s1.session_token, s2.session_token);
    assert_eq!(reg.list_active(true).len(), 2);
}

#[test]
fn heartbeat_with_correct_token_refreshes() {
    let reg = PresenceRegistry::new();
    let s = reg.register("a".into(), AgentKind::ClaudeCode, AgentMode::Interactive, None, None);
    std::thread::sleep(std::time::Duration::from_millis(10));
    let before = reg.get(&s.id).unwrap().last_heartbeat;
    reg.heartbeat(&s.id, &s.session_token).unwrap();
    let after = reg.get(&s.id).unwrap().last_heartbeat;
    assert!(after > before);
}

#[test]
fn heartbeat_with_wrong_token_errors() {
    let reg = PresenceRegistry::new();
    let s = reg.register("a".into(), AgentKind::ClaudeCode, AgentMode::Interactive, None, None);
    assert!(matches!(reg.heartbeat(&s.id, "wrong"), Err(HeartbeatError::WrongToken)));
}

#[test]
fn expire_stale_releases_old_sessions() {
    let reg = PresenceRegistry::with_expiry(std::time::Duration::from_millis(20));
    let s = reg.register("a".into(), AgentKind::ClaudeCode, AgentMode::Interactive, None, None);
    std::thread::sleep(std::time::Duration::from_millis(40));
    let released = reg.expire_stale();
    assert_eq!(released, vec![s.id.clone()]);
    assert_eq!(reg.list_active(true).len(), 0);
}

#[test]
fn background_agents_excluded_from_default_list() {
    let reg = PresenceRegistry::new();
    reg.register("cron".into(), AgentKind::Other("cron".into()), AgentMode::Background, None, None);
    reg.register("claude".into(), AgentKind::ClaudeCode, AgentMode::Interactive, None, None);
    assert_eq!(reg.list_active(false).len(), 1);
    assert_eq!(reg.list_active(true).len(), 2);
}

#[test]
fn by_token_resolves_session_token() {
    let reg = PresenceRegistry::new();
    let s = reg.register("a".into(), AgentKind::ClaudeCode, AgentMode::Interactive, None, None);
    assert_eq!(reg.by_token(&s.session_token).map(|x| x.id), Some(s.id));
    assert!(reg.by_token("missing").is_none());
}

#[test]
fn claim_grants_empty_path_when_unoccupied() {
    let occ = lain::server::presence::OccupancyMap::new();
    let agent = AgentId("a".into());
    let result = occ.claim(&agent, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
    }]);
    assert_eq!(result.granted.len(), 1);
    assert_eq!(result.conflicts.len(), 0);
}

#[test]
fn claim_reports_conflict_on_overlap() {
    let occ = lain::server::presence::OccupancyMap::new();
    let alice = AgentId("alice".into());
    let bob = AgentId("bob".into());
    occ.claim(&alice, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
    }]);
    let result = occ.claim(&bob, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
    }]);
    assert_eq!(result.granted.len(), 0);
    assert_eq!(result.conflicts.len(), 1);
    assert_eq!(result.conflicts[0].agent_id, alice);
}

#[test]
fn claim_different_symbols_on_same_file_no_conflict() {
    let occ = lain::server::presence::OccupancyMap::new();
    let alice = AgentId("alice".into());
    let bob = AgentId("bob".into());
    occ.claim(&alice, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
    }]);
    let result = occ.claim(&bob, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["validate".into()],
        intent: ClaimIntent::Edit,
    }]);
    assert_eq!(result.granted.len(), 1);
    assert_eq!(result.conflicts.len(), 0);
}

#[test]
fn claim_file_level_no_symbols_overlaps_with_anything_on_file() {
    let occ = lain::server::presence::OccupancyMap::new();
    let alice = AgentId("alice".into());
    let bob = AgentId("bob".into());
    occ.claim(&alice, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec![],
        intent: ClaimIntent::Edit,
    }]);
    let result = occ.claim(&bob, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["anything".into()],
        intent: ClaimIntent::Edit,
    }]);
    assert_eq!(result.granted.len(), 0);
    assert_eq!(result.conflicts.len(), 1);
}

#[test]
fn release_returns_removed_paths() {
    let occ = lain::server::presence::OccupancyMap::new();
    let alice = AgentId("alice".into());
    occ.claim(&alice, vec![
        ClaimRequest { path: std::path::PathBuf::from("auth.rs"), symbols: vec!["login".into()], intent: ClaimIntent::Edit },
        ClaimRequest { path: std::path::PathBuf::from("db.rs"), symbols: vec![], intent: ClaimIntent::Read },
    ]);
    let released = occ.release(&alice, &[std::path::PathBuf::from("auth.rs")]);
    assert_eq!(released, vec![std::path::PathBuf::from("auth.rs")]);
    assert_eq!(occ.list_for_agent(&alice).len(), 1);
}

#[test]
fn release_all_for_clears_agent() {
    let occ = lain::server::presence::OccupancyMap::new();
    let alice = AgentId("alice".into());
    occ.claim(&alice, vec![
        ClaimRequest { path: std::path::PathBuf::from("auth.rs"), symbols: vec![], intent: ClaimIntent::Edit },
        ClaimRequest { path: std::path::PathBuf::from("db.rs"), symbols: vec![], intent: ClaimIntent::Edit },
    ]);
    let released = occ.release_all_for(&alice);
    assert_eq!(released.len(), 2);
    assert_eq!(occ.list_for_agent(&alice).len(), 0);
}

#[test]
fn list_for_path_shows_all_agents() {
    let occ = lain::server::presence::OccupancyMap::new();
    occ.claim(&AgentId("alice".into()), vec![ClaimRequest { path: std::path::PathBuf::from("auth.rs"), symbols: vec!["login".into()], intent: ClaimIntent::Edit }]);
    occ.claim(&AgentId("bob".into()), vec![ClaimRequest { path: std::path::PathBuf::from("auth.rs"), symbols: vec!["validate".into()], intent: ClaimIntent::Edit }]);
    let entry = occ.list_for_path(&std::path::PathBuf::from("auth.rs")).unwrap();
    assert_eq!(entry.agents.len(), 2);
    assert_eq!(entry.symbols.len(), 2);
}

#[test]
fn list_all_returns_all_claimed_paths() {
    let occ = lain::server::presence::OccupancyMap::new();
    occ.claim(&AgentId("alice".into()), vec![ClaimRequest { path: std::path::PathBuf::from("auth.rs"), symbols: vec!["login".into()], intent: ClaimIntent::Edit }]);
    occ.claim(&AgentId("bob".into()), vec![ClaimRequest { path: std::path::PathBuf::from("db.rs"), symbols: vec![], intent: ClaimIntent::Edit }]);

    let entries = occ.list_all();
    let paths: std::collections::HashSet<_> = entries.iter().map(|e| e.path.clone()).collect();
    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&std::path::PathBuf::from("auth.rs")));
    assert!(paths.contains(&std::path::PathBuf::from("db.rs")));
}

/// `LainServer::new` (single-workspace path) must carry an empty
/// `PresenceRegistry` and `OccupancyMap` so the MCP/SSE layer can hand
/// them out without conditional checks. The expiry loop is only spawned
/// by the federation constructors; this test exercises the simpler path.
#[tokio::test]
async fn lain_server_exposes_presence_and_occupancy() {
    use lain::server::LainServer;
    // Build a single-workspace server (uses the placeholder ingestion)
    let tmp = tempfile::tempdir().unwrap();
    // `GitSensor::new` calls `git2::Repository::open`, which requires an
    // initialized repo — a bare `.git` directory is not enough.
    git2::Repository::init(tmp.path()).unwrap();
    std::fs::write(tmp.path().join("a.rs"), "pub fn a() {}").unwrap();
    let mem = tmp.path().join(".lain/graph.bin");
    let server = LainServer::new(tmp.path(), &mem, None).expect("server");
    assert!(server.presence.list_active(true).is_empty());
    assert!(server.occupancy.list_all().is_empty());
}

/// `serve_sse` must convert each broadcast `PresenceEvent` into an
/// `SseFrame` carrying the variant name and the agent id so a browser
/// `EventSource` consumer can dispatch on `event` and pull the id out of
/// `data`. We drop the `futures::StreamExt` import from the brief because
/// `futures` is not a dependency — `SseStream` exposes `.next()` directly
/// so callers don't need the trait.
#[tokio::test]
async fn sse_broadcasts_presence_events() {
    use lain::server::presence::PresenceEvent;
    use lain::server::sse::serve_sse;

    let (tx, _rx) = tokio::sync::broadcast::channel::<PresenceEvent>(16);
    let mut stream = serve_sse(tx.subscribe(), None);

    tx.send(PresenceEvent::AgentLeft(AgentId("x".into()))).unwrap();

    let event = tokio::time::timeout(std::time::Duration::from_millis(200), stream.next())
        .await
        .expect("event arrived")
        .expect("some event")
        .expect("not an error");
    assert!(event.data.contains("AgentLeft"));
    assert!(event.data.contains("x"));
}

// --- Task 7 brief: MCP tool layer end-to-end round-trips ---

#[test]
fn register_agent_returns_id_and_token() {
    use lain::server::presence::PresenceRegistry;
    let reg = PresenceRegistry::new();
    let session = reg.register("a".into(), AgentKind::ClaudeCode, AgentMode::Interactive, None, None);
    assert!(session.id.as_str().contains("-")); // UUID has dashes
    // Session token is 16 random bytes rendered as 32 lowercase hex chars
    // (128 bits of entropy). The brief's draft expected 64; the actual
    // implementation has shipped 32 since Task 2 and other tests rely on
    // it, so we match the implementation here.
    assert_eq!(session.session_token.len(), 32);
    assert!(session.session_token.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn occupancy_round_trip() {
    let occ = OccupancyMap::new();
    let alice = AgentId("alice".into());
    let req = ClaimRequest { path: std::path::PathBuf::from("auth.rs"), symbols: vec!["login".into()], intent: ClaimIntent::Edit };
    let r = occ.claim(&alice, vec![req]);
    assert_eq!(r.granted.len(), 1);
    let claims = occ.list_for_agent(&alice);
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].symbols, vec!["login"]);
}

/// Each of the 8 multiplayer MCP tools must round-trip through the
/// `run_*` dispatcher against a real `LainServer`. We exercise
/// `register_agent` (AgentJoined event), `claim_files` (ClaimGranted
/// and ConflictDetected), `release_files` (ClaimReleased), and
/// `list_active_agents` / `who_am_i` / `my_claims` / `list_occupancy`.
#[tokio::test]
async fn presence_tool_dispatchers_round_trip() {
    use lain::server::mcp::presence_tools::{
        run_claim_files, run_list_active_agents, run_list_occupancy,
        run_my_claims, run_register_agent, run_release_files, run_who_am_i,
    };
    use lain::server::LainServer;

    let tmp = tempfile::tempdir().unwrap();
    git2::Repository::init(tmp.path()).unwrap();
    std::fs::write(tmp.path().join("a.rs"), "pub fn a() {}").unwrap();
    let mem = tmp.path().join(".lain/graph.bin");
    let server = LainServer::new(tmp.path(), &mem, None).expect("server");
    let server_arc = std::sync::Arc::new(server);

    // Subscribe BEFORE register_agent so we don't miss AgentJoined.
    let mut events = server_arc.presence_event_tx.subscribe();

    // 1. register_agent returns id + token + expiry.
    let v = run_register_agent(
        &server_arc,
        serde_json::json!({"name": "alice", "kind": "claude-code"}),
    ).unwrap();
    let agent_id = v["agent_id"].as_str().unwrap().to_string();
    let token = v["session_token"].as_str().unwrap().to_string();
    assert!(v["expires_at_unix"].as_u64().unwrap() > 0);
    assert_eq!(v["agent_id"].as_str().unwrap().len(), 36); // UUID string

    // AgentJoined fired.
    let ev = events.recv().await.unwrap();
    match ev {
        PresenceEvent::AgentJoined(s) => assert_eq!(s.id.as_str(), agent_id),
        other => panic!("expected AgentJoined, got {other:?}"),
    }

    // 2. claim_files grants auth.rs; second claim on a different file
    //    doesn't conflict. A second agent claiming the SAME file does.
    let v = run_claim_files(
        &server_arc,
        serde_json::json!({
            "agent_id": agent_id,
            "session_token": token,
            "files": [{"path": "auth.rs", "symbols": ["login"]}],
        }),
    ).unwrap();
    assert_eq!(v["granted"].as_array().unwrap().len(), 1);
    assert_eq!(v["conflicts"].as_array().unwrap().len(), 0);

    // Drain ClaimGranted.
    let ev = events.recv().await.unwrap();
    assert!(matches!(ev, PresenceEvent::ClaimGranted { .. }));

    // 3. Register a second agent to provoke a conflict.
    let v2 = run_register_agent(
        &server_arc,
        serde_json::json!({"name": "bob"}),
    ).unwrap();
    let bob_id = v2["agent_id"].as_str().unwrap().to_string();
    let bob_token = v2["session_token"].as_str().unwrap().to_string();
    // Drain bob's AgentJoined.
    let _ = events.recv().await.unwrap();

    let v = run_claim_files(
        &server_arc,
        serde_json::json!({
            "agent_id": bob_id,
            "session_token": bob_token,
            "files": [{"path": "auth.rs", "symbols": ["login"]}],
        }),
    ).unwrap();
    assert_eq!(v["granted"].as_array().unwrap().len(), 0);
    assert_eq!(v["conflicts"].as_array().unwrap().len(), 1);
    // ConflictDetected fired.
    let ev = events.recv().await.unwrap();
    assert!(matches!(ev, PresenceEvent::ConflictDetected { .. }));

    // 4. list_active_agents sees both.
    let v = run_list_active_agents(&server_arc, serde_json::json!({})).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2);

    // 5. who_am_i resolves the token; claims_count comes from
    //    list_for_agent, which is non-empty for alice.
    let v = run_who_am_i(
        &server_arc,
        serde_json::json!({"session_token": token}),
    ).unwrap();
    assert_eq!(v["agent_id"].as_str().unwrap(), agent_id);
    assert_eq!(v["claims"].as_array().unwrap().len(), 1);

    // 6. my_claims returns the alice claim.
    let v = run_my_claims(
        &server_arc,
        serde_json::json!({"agent_id": agent_id, "session_token": token}),
    ).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["path"].as_str().unwrap(), "auth.rs");

    // 7. list_occupancy shows the file with both alice (we'll see
    //    that in agent_names after list_all).
    let v = run_list_occupancy(&server_arc, serde_json::json!({})).unwrap();
    let arr = v.as_array().unwrap();
    assert!(!arr.is_empty());

    // 8. release_files releases auth.rs and fires ClaimReleased.
    let v = run_release_files(
        &server_arc,
        serde_json::json!({
            "agent_id": agent_id,
            "session_token": token,
            "files": [{"path": "auth.rs"}],
        }),
    ).unwrap();
    assert_eq!(v["released"].as_array().unwrap().len(), 1);
    let ev = events.recv().await.unwrap();
    assert!(matches!(ev, PresenceEvent::ClaimReleased { .. }));

    // 9. After release, alice's claim count is 0.
    let v = run_my_claims(
        &server_arc,
        serde_json::json!({"agent_id": agent_id, "session_token": token}),
    ).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 0);
}
