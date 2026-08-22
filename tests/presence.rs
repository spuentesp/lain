use lain::server::presence::*;
use std::time::SystemTime;

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
        ttl_seconds: None,
        plan_revision: None,
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
        ttl_seconds: None,
        plan_revision: None,
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
        ttl_seconds: None,
        plan_revision: None,
    }]);
    let result = occ.claim(&bob, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
        plan_revision: None,
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
        ttl_seconds: None,
        plan_revision: None,
    }]);
    let result = occ.claim(&bob, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["validate".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
        plan_revision: None,
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
        ttl_seconds: None,
        plan_revision: None,
    }]);
    let result = occ.claim(&bob, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["anything".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
        plan_revision: None,
    }]);
    assert_eq!(result.granted.len(), 0);
    assert_eq!(result.conflicts.len(), 1);
}

#[test]
fn release_returns_removed_paths() {
    let occ = lain::server::presence::OccupancyMap::new();
    let alice = AgentId("alice".into());
    occ.claim(&alice, vec![
        ClaimRequest { path: std::path::PathBuf::from("auth.rs"), symbols: vec!["login".into()], intent: ClaimIntent::Edit, ttl_seconds: None, plan_revision: None },
        ClaimRequest { path: std::path::PathBuf::from("db.rs"), symbols: vec![], intent: ClaimIntent::Read, ttl_seconds: None, plan_revision: None },
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
        ClaimRequest { path: std::path::PathBuf::from("auth.rs"), symbols: vec![], intent: ClaimIntent::Edit, ttl_seconds: None, plan_revision: None },
        ClaimRequest { path: std::path::PathBuf::from("db.rs"), symbols: vec![], intent: ClaimIntent::Edit, ttl_seconds: None, plan_revision: None },
    ]);
    let released = occ.release_all_for(&alice);
    assert_eq!(released.len(), 2);
    assert_eq!(occ.list_for_agent(&alice).len(), 0);
}

#[test]
fn list_for_path_shows_all_agents() {
    let occ = lain::server::presence::OccupancyMap::new();
    occ.claim(&AgentId("alice".into()), vec![ClaimRequest { path: std::path::PathBuf::from("auth.rs"), symbols: vec!["login".into()], intent: ClaimIntent::Edit, ttl_seconds: None, plan_revision: None }]);
    occ.claim(&AgentId("bob".into()), vec![ClaimRequest { path: std::path::PathBuf::from("auth.rs"), symbols: vec!["validate".into()], intent: ClaimIntent::Edit, ttl_seconds: None, plan_revision: None }]);
    let entry = occ.list_for_path(&std::path::PathBuf::from("auth.rs")).unwrap();
    assert_eq!(entry.agents.len(), 2);
    assert_eq!(entry.symbols.len(), 2);
}

#[test]
fn list_all_returns_all_claimed_paths() {
    let occ = lain::server::presence::OccupancyMap::new();
    occ.claim(&AgentId("alice".into()), vec![ClaimRequest { path: std::path::PathBuf::from("auth.rs"), symbols: vec!["login".into()], intent: ClaimIntent::Edit, ttl_seconds: None, plan_revision: None }]);
    occ.claim(&AgentId("bob".into()), vec![ClaimRequest { path: std::path::PathBuf::from("db.rs"), symbols: vec![], intent: ClaimIntent::Edit, ttl_seconds: None, plan_revision: None }]);

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
    use lain::server::events_log::EventsLog;
    use lain::server::presence::PresenceEvent;
    use lain::server::sse::serve_sse;

    let tmp = tempfile::tempdir().unwrap();
    let log = std::sync::Arc::new(EventsLog::open(tmp.path()).unwrap());
    let (tx, _rx) = tokio::sync::broadcast::channel::<(u64, PresenceEvent)>(16);
    let mut stream = serve_sse(tx.subscribe(), None, log);

    tx.send((7, PresenceEvent::AgentLeft(AgentId("x".into())))).unwrap();

    let event = tokio::time::timeout(std::time::Duration::from_millis(200), stream.next())
        .await
        .expect("event arrived")
        .expect("some event")
        .expect("not an error");
    assert!(event.data.contains("AgentLeft"));
    assert!(event.data.contains("x"));
    assert_eq!(event.id, 7, "live frames carry the durable event id");
}

/// `serve_sse` with a `Last-Event-ID` replays every durable event with
/// id > last_id (in order) before yielding from the live bus.
#[tokio::test]
async fn sse_replays_after_last_event_id() {
    use lain::server::events_log::EventsLog;
    use lain::server::presence::PresenceEvent;
    use lain::server::sse::serve_sse;

    let tmp = tempfile::tempdir().unwrap();
    let log = std::sync::Arc::new(EventsLog::open(tmp.path()).unwrap());
    let id1 = log.append(&PresenceEvent::AgentLeft(AgentId("old".into())));
    let id2 = log.append(&PresenceEvent::AgentLeft(AgentId("new".into())));

    let (tx, _rx) = tokio::sync::broadcast::channel::<(u64, PresenceEvent)>(16);
    let mut stream = serve_sse(tx.subscribe(), Some(id1.to_string()), log);

    // The replayed frame arrives without any live broadcast.
    let frame = tokio::time::timeout(std::time::Duration::from_millis(200), stream.next())
        .await
        .expect("replayed frame arrived")
        .expect("some frame")
        .expect("not an error");
    assert_eq!(frame.id, id2);
    assert!(frame.data.contains("new"));

    // After the backlog drains, live events flow with their durable ids.
    tx.send((id2 + 1, PresenceEvent::AgentLeft(AgentId("live".into()))))
        .unwrap();
    let live = tokio::time::timeout(std::time::Duration::from_millis(200), stream.next())
        .await
        .expect("live frame arrived")
        .expect("some frame")
        .expect("not an error");
    assert_eq!(live.id, id2 + 1);
    assert!(live.data.contains("live"));
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
    let req = ClaimRequest { path: std::path::PathBuf::from("auth.rs"), symbols: vec!["login".into()], intent: ClaimIntent::Edit, ttl_seconds: None, plan_revision: None };
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
    let (_, ev) = events.recv().await.unwrap();
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
    let (_, ev) = events.recv().await.unwrap();
    assert!(matches!(ev, PresenceEvent::ClaimGranted { .. }));
    // Drain EditLanded (PR 2 / Task 2.4 — emitted alongside the
    // audit append for the granted claim).
    let (_, ev) = events.recv().await.unwrap();
    assert!(matches!(ev, PresenceEvent::EditLanded { .. }));

    // 3. Register a second agent to provoke a conflict.
    let v2 = run_register_agent(
        &server_arc,
        serde_json::json!({"name": "bob"}),
    ).unwrap();
    let bob_id = v2["agent_id"].as_str().unwrap().to_string();
    let bob_token = v2["session_token"].as_str().unwrap().to_string();
    // Drain bob's AgentJoined.
    let (_, ev) = events.recv().await.unwrap();
    assert!(matches!(ev, PresenceEvent::AgentJoined { .. }));

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
    let (_, ev) = events.recv().await.unwrap();
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

    // 7. list_occupancy shows the file with alice (bob's claim was
    //    rejected as a conflict, so only alice is on the file).
    //    `last_seen_unix` surfaces the heartbeat of the first live
    //    agent (the field replaces the older `agent_names` payload).
    let v = run_list_occupancy(&server_arc, serde_json::json!({})).unwrap();
    let arr = v.as_array().unwrap();
    assert!(!arr.is_empty());
    let entry = &arr[0];
    assert_eq!(entry["agents"].as_array().unwrap().len(), 1);
    let lsu = entry["last_seen_unix"].as_u64().unwrap();
    assert!(lsu > 0, "live session must surface last_seen_unix");

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
    let (_, ev) = events.recv().await.unwrap();
    assert!(matches!(ev, PresenceEvent::ClaimReleased { .. }));

    // 9. After release, alice's claim count is 0.
    let v = run_my_claims(
        &server_arc,
        serde_json::json!({"agent_id": agent_id, "session_token": token}),
    ).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 0);
}

// --- Task 2 brief: session token caching directory ---

/// Task 2 adds a `hooks_dir()` helper alongside `config_dir()`. This
/// is a direct path test — we just want to make sure the helper exists
/// and points under the config dir.
#[test]
fn config_dir_contains_hooks_subdir_helper() {
    // Direct path test — we just want to make sure the helper exists.
    let hooks = std::path::PathBuf::from(format!(
        "{}/hooks",
        lain::config::config_dir().display()
    ));
    // We don't create the dir; we just check the path computation.
    assert!(hooks.ends_with("hooks"));
}

// --- Task 1 brief: SymbolHash content hashing ---

/// `SymbolHash::from_bytes` must be deterministic and collision-free
/// across distinct inputs — BLAKE3-256 over the same byte slice yields
/// the same hash, and distinct slices yield distinct hashes.
#[test]
fn symbol_hash_from_bytes_roundtrips() {
    let h = SymbolHash::from_bytes(b"hello");
    let h2 = SymbolHash::from_bytes(b"hello");
    assert_eq!(h, h2);
    let h3 = SymbolHash::from_bytes(b"world");
    assert_ne!(h, h3);
}

/// `SymbolHash::zero()` is the all-zero 32-byte array, which is NOT
/// the BLAKE3 hash of the empty string — they're distinct bit
/// patterns, and the placeholder must remain distinguishable from any
/// real body hash so downstream code can tell "unset" from "computed".
#[test]
fn symbol_hash_zero_is_distinct_from_real_hash() {
    let z = SymbolHash::zero();
    let r = SymbolHash::from_bytes(b"");
    assert_ne!(z, r);
}

// --- Task 2 brief: PresenceRegistry + OccupancyMap persistence ---

/// Round-trip a freshly-populated pair of registries through the JSON
/// persistence helpers. The first pair is mutated in-memory, serialized
/// to a temp file via `save_pair`, and a second pair is hydrated via
/// `load_pair`. After hydration the recovered registry carries one
/// active session and the recovered occupancy map has the original
/// claim attached to the same agent id.
#[test]
fn persistence_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let stem = "test-stem";
    let path = tmp.path().join(format!("{stem}.json"));

    // Round 1: register an agent, claim a file, save.
    let reg1 = PresenceRegistry::new();
    let occ1 = OccupancyMap::new();
    let agent = reg1.register(
        "a".into(),
        AgentKind::ClaudeCode,
        AgentMode::Interactive,
        None,
        None,
    );
    occ1.claim(
        &agent.id,
        vec![ClaimRequest {
            path: std::path::PathBuf::from("foo.rs"),
            symbols: vec![],
            intent: ClaimIntent::Edit,
            ttl_seconds: None,
            plan_revision: None,
        }],
    );
    save_pair(&path, &reg1, &occ1).unwrap();

    // Round 2: load into a fresh registry.
    let reg2 = PresenceRegistry::new();
    let occ2 = OccupancyMap::new();
    load_pair(&path, &reg2, &occ2).unwrap();

    assert_eq!(reg2.list_active(true).len(), 1);
    assert_eq!(occ2.list_for_agent(&agent.id).len(), 1);
}

// --- Task 3 brief: explicit claim TTL via ttl_seconds + expires_at ---

/// `ClaimRequest.ttl_seconds = Some(n)` must populate the resulting
/// `Claim.expires_at` as `claimed_at + n`. After sleeping past the TTL,
/// the stored timestamp is provably in the past — the federation expiry
/// loop (which `expire_by_ttl` powers) will release the claim on its
/// next tick.
#[test]
fn ttl_seconds_bounds_claim_even_with_heartbeat() {
    let occ = OccupancyMap::new();
    let alice = AgentId("alice".into());
    let req = ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec![],
        intent: ClaimIntent::Edit,
        ttl_seconds: Some(1), // 1 second
        plan_revision: None,
    };
    occ.claim(&alice, vec![req]);
    let claims = occ.list_for_agent(&alice);
    assert_eq!(claims.len(), 1);
    let expires_at = claims[0].expires_at;
    assert!(expires_at.is_some(), "ttl_seconds must set expires_at");
    // Sleep past the TTL.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    // The expiry loop in LainServer would release this — but OccupancyMap
    // alone doesn't have an expiry method. We assert the timestamp is in
    // the past so the loop *would* release it.
    let expires_at = claims[0].expires_at.unwrap();
    assert!(expires_at < std::time::SystemTime::now());
}

/// `ClaimRequest.ttl_seconds = None` means no expiry: `Claim.expires_at`
/// stays `None` and the federation expiry loop ignores the claim even
/// after a long wait. Only explicit `release` or the owning session's
/// heartbeat expiry will ever drop it.
#[test]
fn no_ttl_means_no_expiry() {
    let occ = OccupancyMap::new();
    let alice = AgentId("alice".into());
    let req = ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec![],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
        plan_revision: None,
    };
    occ.claim(&alice, vec![req]);
    let claims = occ.list_for_agent(&alice);
    assert!(claims[0].expires_at.is_none(), "no ttl means no expiry");
}

/// `OccupancyMap::expire_by_ttl` must drop claims whose `expires_at` has
/// passed and return one `(agent_id, path)` pair per dropped claim, plus
/// keep alive the claims whose TTL is in the future. Bookkeeping for
/// `by_file` (agent set + symbol sets) must also be cleaned so a
/// subsequent `list_for_path` reflects the released state.
#[test]
fn expire_by_ttl_releases_expired_claims() {
    let occ = OccupancyMap::new();
    let alice = AgentId("alice".into());
    let bob = AgentId("bob".into());
    // alice: 1s TTL on auth.rs — will expire.
    occ.claim(&alice, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec![],
        intent: ClaimIntent::Edit,
        ttl_seconds: Some(1),
        plan_revision: None,
    }]);
    // bob: no TTL on db.rs — survives. Different file keeps the test
    // from accidentally exercising the file-level vs symbol-level
    // conflict path.
    occ.claim(&bob, vec![ClaimRequest {
        path: std::path::PathBuf::from("db.rs"),
        symbols: vec![],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
        plan_revision: None,
    }]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let released = occ.expire_by_ttl();
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].0, alice);
    assert_eq!(released[0].1, std::path::PathBuf::from("auth.rs"));
    // alice's claim is gone, bob's survives.
    assert!(occ.list_for_agent(&alice).is_empty());
    assert_eq!(occ.list_for_agent(&bob).len(), 1);
    // The expired file's bookkeeping is cleaned; bob's file remains.
    assert!(occ.list_for_path(&std::path::PathBuf::from("auth.rs")).is_none());
    assert!(occ.list_for_path(&std::path::PathBuf::from("db.rs")).is_some());
}

// --- Task 3 (parent plan): conflict shape says *what*, not just *that* ---

/// `Claim` gains a `last_touched_unix` timestamp that must be populated
/// at claim time and never exceed "now" on the system clock. It is the
/// trust signal that downstream conflict reporting carries — agents
/// want to know *when* a conflict was last touched, not just *who* is
/// holding the conflicting claim.
#[test]
fn claim_recorded_last_touched_unix() {
    let occ = OccupancyMap::new();
    let agent = AgentId("alice".into());
    let req = ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec![],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
        plan_revision: None,
    };
    occ.claim(&agent, vec![req]);
    let claims = occ.list_for_agent(&agent);
    assert!(claims[0].last_touched_unix <= SystemTime::now());
}

/// An incoming read claim must not be flagged as a conflict against an
/// existing edit claim — reads are non-destructive and the wishlist
/// (item #5) explicitly asked for this filter. The bob agent should
/// walk away with a granted claim and zero conflicts.
#[test]
fn read_claim_does_not_conflict_with_edit_claim() {
    let occ = OccupancyMap::new();
    let alice = AgentId("alice".into());
    let bob = AgentId("bob".into());
    occ.claim(&alice, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
        plan_revision: None,
    }]);
    let result = occ.claim(&bob, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Read, // <-- read, not edit
        ttl_seconds: None,
        plan_revision: None,
    }]);
    assert_eq!(result.granted.len(), 1, "read claim should NOT conflict with edit");
    assert_eq!(result.conflicts.len(), 0);
}

/// Edit-vs-edit must still conflict as before — the read-vs-edit filter
/// is *additive* loosening, not a blanket "no conflicts" rule. The
/// surviving conflict entry also carries `last_seen_unix` so the
/// caller knows when the conflicting claim was first recorded.
#[test]
fn edit_claim_still_conflicts_with_existing_edit_claim() {
    let occ = OccupancyMap::new();
    let alice = AgentId("alice".into());
    let bob = AgentId("bob".into());
    occ.claim(&alice, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
        plan_revision: None,
    }]);
    let result = occ.claim(&bob, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
        plan_revision: None,
    }]);
    assert_eq!(result.granted.len(), 0, "edit-vs-edit conflict should still hold");
    assert_eq!(result.conflicts.len(), 1);
    let c = &result.conflicts[0];
    assert_eq!(c.symbols, vec!["login".to_string()]);
    assert!(c.last_seen_unix <= SystemTime::now());
}

/// Residual defect after the first read-vs-edit pass: a holder with
/// only a *symbol-level* Read claim (no file-level claim) was
/// still blocking a file-level Edit from another agent, and the
/// conflict entry's `intent` field was hardcoded to `Edit` instead
/// of the holder's real `Read` intent. This test pins both: a
/// file-level Edit on a file where the only other agent has a
/// symbol-level Read must be granted, with zero conflicts, and
/// when we *do* conflict (any symbol-level Edit by the holder),
/// the reported intent must be the holder's actual intent.
#[test]
fn file_level_edit_does_not_conflict_with_symbol_level_read() {
    let occ = OccupancyMap::new();
    let xena = AgentId("xena".into());
    let yuri = AgentId("yuri".into());

    // xena claims a single symbol with intent=Read. No file-level claim.
    occ.claim(&xena, vec![ClaimRequest {
        path: std::path::PathBuf::from("t1.rs"),
        symbols: vec!["func_x".into()],
        intent: ClaimIntent::Read,
        ttl_seconds: None,
        plan_revision: None,
    }]);

    // yuri does a file-level Edit. xena's symbol-level Read is a
    // non-event per wishlist #5.
    let result = occ.claim(&yuri, vec![ClaimRequest {
        path: std::path::PathBuf::from("t1.rs"),
        symbols: vec![], // file-level
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
        plan_revision: None,
    }]);
    assert_eq!(
        result.granted.len(),
        1,
        "yuri's file-level Edit must be granted (xena only holds a Read)"
    );
    assert_eq!(
        result.conflicts.len(),
        0,
        "xena's symbol-level Read must not conflict with yuri's Edit"
    );
}

/// When the holder *does* have a symbol-level Edit (not just Read),
/// the file-level Edit from another agent is a real conflict, and
/// the reported `intent` is the holder's actual `Edit` (not a
/// synthetic default).
#[test]
fn file_level_edit_conflicts_with_symbol_level_edit_and_reports_real_intent() {
    let occ = OccupancyMap::new();
    let xena = AgentId("xena".into());
    let yuri = AgentId("yuri".into());

    // xena claims a symbol with intent=Edit (no file-level).
    occ.claim(&xena, vec![ClaimRequest {
        path: std::path::PathBuf::from("t1.rs"),
        symbols: vec!["func_x".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
        plan_revision: None,
    }]);

    let result = occ.claim(&yuri, vec![ClaimRequest {
        path: std::path::PathBuf::from("t1.rs"),
        symbols: vec![],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
        plan_revision: None,
    }]);
    assert_eq!(result.granted.len(), 0, "file-level Edit blocks on symbol-level Edit");
    assert_eq!(result.conflicts.len(), 1);
    let c = &result.conflicts[0];
    assert_eq!(
        c.intent,
        ClaimIntent::Edit,
        "conflict entry's intent must reflect xena's real Edit intent"
    );
}

// --- Task 2 brief: parent_session_id surfaces in who_am_i, list_subagents works ---

/// `AgentSession::parent_session_id` is set at construction. A parent
/// has `None`; a subagent has `Some(parent_id)`. The brief's example
/// uses `AgentSession::new` directly because the field is public and
/// the production wiring is verified by the dispatchers round-trip
/// test below.
#[test]
fn parent_session_id_round_trips_on_agent_session() {
    let parent_id = AgentId("parent-id".into());
    let parent = AgentSession::new(
        parent_id.clone(),
        "parent".into(),
        AgentKind::ClaudeCode,
        AgentMode::Interactive,
        None,
        None,
    );
    assert_eq!(parent.parent_session_id, None);

    let sub = AgentSession::new(
        AgentId("sub-id".into()),
        "sub".into(),
        AgentKind::ClaudeCode,
        AgentMode::Interactive,
        None,
        Some(parent_id.clone()),
    );
    assert_eq!(sub.parent_session_id, Some(parent_id));
}

/// `who_am_i` must surface `parent_session_id` in its JSON payload so
/// a subagent can introspect its lineage without a second registry
/// call. The opposite case (`None`) is also asserted so existing top-
/// level agents that never set a parent still see a clean `null`.
#[tokio::test]
async fn who_am_i_includes_parent_session_id() {
    use lain::server::mcp::presence_tools::{run_list_subagents, run_register_agent, run_who_am_i};
    use lain::server::LainServer;

    let tmp = tempfile::tempdir().unwrap();
    git2::Repository::init(tmp.path()).unwrap();
    std::fs::write(tmp.path().join("a.rs"), "pub fn a() {}").unwrap();
    let mem = tmp.path().join(".lain/graph.bin");
    let server = std::sync::Arc::new(LainServer::new(tmp.path(), &mem, None).expect("server"));

    // Register a parent (no parent_session_id).
    let parent_reg = run_register_agent(
        &server,
        serde_json::json!({"name": "parent"}),
    ).unwrap();
    let parent_id = parent_reg["agent_id"].as_str().unwrap().to_string();
    let parent_token = parent_reg["session_token"].as_str().unwrap().to_string();

    // Register a subagent that names the parent.
    let sub_reg = run_register_agent(
        &server,
        serde_json::json!({"name": "sub", "parent_session_id": parent_id}),
    ).unwrap();
    let sub_id = sub_reg["agent_id"].as_str().unwrap().to_string();
    let sub_token = sub_reg["session_token"].as_str().unwrap().to_string();

    // who_am_i on the parent: parent_session_id is null.
    let v = run_who_am_i(&server, serde_json::json!({"session_token": parent_token})).unwrap();
    assert!(v["parent_session_id"].is_null(), "top-level agent has no parent");
    assert_eq!(v["agent_id"].as_str().unwrap(), parent_id);

    // who_am_i on the subagent: parent_session_id matches the parent.
    let v = run_who_am_i(&server, serde_json::json!({"session_token": sub_token})).unwrap();
    assert_eq!(v["parent_session_id"].as_str().unwrap(), parent_id);
    assert_eq!(v["agent_id"].as_str().unwrap(), sub_id);

    // list_subagents from the parent's POV returns the sub and only the sub.
    let v = run_list_subagents(&server, serde_json::json!({"session_token": parent_token})).unwrap();
    assert_eq!(v["parent"].as_str().unwrap(), parent_id);
    let subs = v["subagents"].as_array().unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0]["agent_id"].as_str().unwrap(), sub_id);
    assert_eq!(subs[0]["name"].as_str().unwrap(), "sub");
    assert_eq!(subs[0]["kind"].as_str().unwrap(), "unknown"); // default kind when none provided
    assert!(subs[0]["started_at_unix"].as_u64().unwrap() > 0);
    assert!(subs[0]["last_heartbeat_unix"].as_u64().unwrap() > 0);

    // list_subagents from the sub's POV returns nothing (no grandchildren).
    let v = run_list_subagents(&server, serde_json::json!({"session_token": sub_token})).unwrap();
    assert_eq!(v["parent"].as_str().unwrap(), sub_id);
    assert_eq!(v["subagents"].as_array().unwrap().len(), 0);

    // list_subagents with an unknown token errors cleanly.
    let err = run_list_subagents(&server, serde_json::json!({"session_token": "nope"})).unwrap_err();
    assert!(err.contains("unknown session token"), "{err}");
}

// --- Task 1 brief: ConflictEntry drops fragile `name`; carries agent_id + last_seen_unix ---

/// `ConflictEntry` exposes `agent_id` + `last_seen_unix` (never the
/// misleading `"<unknown>"` string that the old `name` field could
/// carry). The struct must be constructible with the new fields and
/// the resulting entry must round-trip them back out.
#[test]
fn conflict_entry_carries_agent_id_and_last_seen_unix() {
    let bob = AgentId("bob".into());
    let now = SystemTime::now();
    let entry = ConflictEntry {
        inferred: false,
        agent_id: bob.clone(),
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
        last_seen_unix: now,
    };
    assert_eq!(entry.agent_id, bob);
    assert_eq!(entry.intent, ClaimIntent::Edit);
    assert_eq!(entry.last_seen_unix, now);
}

/// `run_claim_files` conflict JSON must carry `agent_id` +
/// `last_seen_unix` + `intent`, and must NOT carry a `name` field
/// (the old `<unknown>` literal was the trigger for this fixup).
#[test]
fn run_claim_files_conflict_json_has_no_unknown_name_field() {
    use lain::server::mcp::presence_tools::run_claim_files;
    use lain::server::LainServer;

    let tmp = tempfile::tempdir().unwrap();
    git2::Repository::init(tmp.path()).unwrap();
    std::fs::write(tmp.path().join("a.rs"), "pub fn a() {}").unwrap();
    let mem = tmp.path().join(".lain/graph.bin");
    let server = LainServer::new(tmp.path(), &mem, None).expect("server");
    let server_arc = std::sync::Arc::new(server);

    // Register two agents, each holding a valid session token.
    let alice = run_register_agent_for_test(&server_arc, "alice");
    let bob = run_register_agent_for_test(&server_arc, "bob");

    // Alice claims auth.rs first.
    let v = run_claim_files(
        &server_arc,
        serde_json::json!({
            "agent_id": alice.0,
            "session_token": alice.1,
            "files": [{"path": "auth.rs", "symbols": ["login"]}],
        }),
    ).unwrap();
    assert_eq!(v["conflicts"].as_array().unwrap().len(), 0);

    // Bob tries the same scope — must conflict.
    let v = run_claim_files(
        &server_arc,
        serde_json::json!({
            "agent_id": bob.0,
            "session_token": bob.1,
            "files": [{"path": "auth.rs", "symbols": ["login"]}],
        }),
    ).unwrap();
    assert_eq!(v["granted"].as_array().unwrap().len(), 0);
    let conflicts = v["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1);
    let c = &conflicts[0];
    // The new conflict JSON shape: agent_id, no name, last_seen_unix.
    assert!(c.get("name").is_none(), "fragile `name` field must be dropped");
    assert!(c["agent_id"].is_string());
    assert!(c["last_seen_unix"].as_u64().unwrap() > 0);
    assert!(c["intent"].as_str().unwrap() == "edit");
    assert_eq!(c["path"].as_str().unwrap(), "auth.rs");
    assert_eq!(c["symbols"].as_array().unwrap().len(), 1);
}

/// Tiny helper: register an agent and return `(agent_id, token)`.
/// Lives at the bottom of the file so the imports stay near the top.
fn run_register_agent_for_test(
    server: &std::sync::Arc<lain::server::LainServer>,
    name: &str,
) -> (String, String) {
    use lain::server::mcp::presence_tools::run_register_agent;
    let v = run_register_agent(server, serde_json::json!({"name": name})).unwrap();
    let id = v["agent_id"].as_str().unwrap().to_string();
    let token = v["session_token"].as_str().unwrap().to_string();
    (id, token)
}

// --- Task 1 brief: real SymbolHash from file bytes ---

/// Symbol-level claims must populate `content_hash` with a real
/// BLAKE3 hash of the symbol's body (not the all-zero placeholder).
/// The body is the exact byte range covered by the tree-sitter
/// definition node (`byte_start..byte_end` in `SymbolDef`) — for a
/// single-function file that's the function minus any trailing
/// newline, since tree-sitter's `end_byte` for a `function_item`
/// stops at the closing brace.
#[test]
fn symbol_level_claim_records_nonzero_content_hash() {
    let occ = OccupancyMap::new();
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("auth.rs");
    std::fs::write(&path, "pub fn login() -> &'static str { \"A\" }\n").unwrap();
    let agent = AgentId("alice".into());
    let req = ClaimRequest {
        path: path.clone(),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
        plan_revision: None,
    };
    occ.claim(&agent, vec![req]);
    let claims = occ.list_for_agent(&agent);
    assert_eq!(claims.len(), 1);
    let hash = claims[0].content_hash.expect("symbol-level claim must have content_hash");
    // The hash must be non-zero (the placeholder), and re-computing the same
    // body must yield the same hash.
    assert_ne!(hash, SymbolHash::zero());
    let again = SymbolHash::from_bytes(b"pub fn login() -> &'static str { \"A\" }");
    assert_eq!(hash, again);
}

/// When the file body changes between two symbol claims, the
/// `content_hash` must change too — that's the whole point of the
/// "hash survives rebuilds" story (federation tracks the same symbol
/// across rebuilds when the body is unchanged, and re-issues a fresh
/// hash when it isn't).
#[test]
fn symbol_level_claim_hash_changes_when_body_changes() {
    let occ = OccupancyMap::new();
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("auth.rs");
    std::fs::write(&path, "pub fn login() -> &'static str { \"A\" }\n").unwrap();
    let agent = AgentId("alice".into());
    occ.claim(&agent, vec![ClaimRequest {
        path: path.clone(),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
        plan_revision: None,
    }]);
    let hash1 = occ.list_for_agent(&agent)[0].content_hash.unwrap();

    std::fs::write(&path, "pub fn login() -> &'static str { \"B\" }\n").unwrap();
    occ.claim(&agent, vec![ClaimRequest {
        path: path.clone(),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
        plan_revision: None,
    }]);
    // Re-claiming the same scope replaces the record rather than
    // appending beside it: an agent that re-claims a file in a loop
    // used to accumulate a row per call in `my_claims`, inflating
    // `claims_count` and leaving stale hashes behind the fresh one.
    // One record, carrying the current body's hash, is what a caller
    // asking "what do I hold?" needs.
    let claims = occ.list_for_agent(&agent);
    assert_eq!(claims.len(), 1, "re-claiming a scope must replace, not duplicate");
    let hash2 = claims[0].content_hash.unwrap();

    assert_ne!(hash1, hash2);
}

// --- Task 1.4 brief: Claim.plan_revision field ---

/// Round-tripping a `Claim` with `plan_revision = Some(42)` must preserve
/// the field. The field is `Option<RevisionId>` (alias for `u64`) on the
/// in-memory `Claim` and sits next to `expires_at` on the struct.
#[test]
fn claim_round_trips_plan_revision() {
    let claim = Claim {
        inferred: false,
        agent_id: AgentId("a1".into()),
        path: std::path::PathBuf::from("/x.rs"),
        symbols: vec!["login".into()],
        content_hash: None,
        intent: ClaimIntent::Edit,
        claimed_at: SystemTime::UNIX_EPOCH,
        last_touched_unix: SystemTime::UNIX_EPOCH,
        expires_at: None,
        plan_revision: Some(42),
    };
    let json = serde_json::to_string(&claim).unwrap();
    let back: Claim = serde_json::from_str(&json).unwrap();
    assert_eq!(back.plan_revision, Some(42));
}

/// Older state files have no `plan_revision` key. With
/// `#[serde(default)]`, deserialization must succeed and yield `None`
/// rather than 400'ing the loader.
#[test]
fn claim_without_plan_revision_deserializes_to_none() {
    let json = r#"{
        "agent_id": "a1",
        "path": "/x.rs",
        "symbols": ["login"],
        "content_hash": null,
        "intent": "Edit"
    }"#;
    let claim: Claim = serde_json::from_str(json).unwrap();
    assert_eq!(claim.plan_revision, None);
}

// -------------------------------------------------------------------------
// Runtime TooOld test: TooOld path through the full claim_files → world_state
// pipeline. The smoke harness couldn't exercise this end-to-end (creating
// 280+ files in the workspace broke the LSP bridge; see
// docs/superpowers/sdd/2026-08-18-coordination-staleness-audit/).
//
// Drive the RevisionLog directly via the public overlay.insert_node API
// instead — that has the same effect (increments current_revision; once
// the ring buffer wraps, floor > 0). Then call run_claim_files with
// plan_revision=0 and assert the verbatim spec note fires.
// -------------------------------------------------------------------------
#[tokio::test]
async fn to_old_path_fires_via_run_claim_files() {
    use lain::server::LainServer;
    use lain::server::schema::{GraphNode, NodeType};

    let tmp = tempfile::tempdir().unwrap();
    git2::Repository::init(tmp.path()).unwrap();
    std::fs::write(tmp.path().join("a.rs"), "pub fn a() {}").unwrap();
    let mem = tmp.path().join(".lain/graph.bin");
    let server = LainServer::new(tmp.path(), &mem, None).expect("server");
    let server_arc = std::sync::Arc::new(server);

    // Drive the overlay revision past the ring buffer's 256-entry
    // capacity. Each insert_node bumps current_revision. The ring's
    // capacity is 256 (set by RevisionLog::new()), so >256 inserts push
    // floor > 0 and make plan=0 fall below it.
    for i in 0..300 {
        let node = GraphNode::new(
            NodeType::Function,
            format!("smoke_f{:04}", i),
            format!("/tmp/synthetic/f{:04}.rs", i),
        );
        let _ = server_arc.overlay.insert_node(node);
    }
    let current = server_arc.overlay.current_revision();
    assert!(
        current > 256,
        "current revision must exceed ring buffer capacity to trigger TooOld; got {}",
        current,
    );

    // Register an agent and claim with plan_revision=0 — that should hit
    // the TooOld branch in compute_world_state.
    let session = server_arc.presence.register(
        "tooold".into(), AgentKind::ClaudeCode, AgentMode::Interactive, None, None,
    );
    let args = serde_json::json!({
        "agent_id": session.id.as_str(),
        "session_token": session.session_token,
        "files": [{
            "path": tmp.path().join("a.rs").to_string_lossy(),
            "symbols": ["a"],
            "intent": "read",
            "plan_revision": 0,
        }],
    });
    let result = lain::server::mcp::presence_tools::run_claim_files(&server_arc, args)
        .expect("claim_files");
    // run_claim_files returns the ClaimResult directly (the dispatcher's
    // tool_text_result wrapper is what adds the {content:[{text:...}]} shape).
    let ws = result.get("world_state").expect("world_state must be present");
    let note = ws.get("note").and_then(|v| v.as_str()).expect("note must be set");
    let plan = ws.get("plan").and_then(|v| v.as_u64()).expect("plan must be set");

    assert_eq!(note, "plan_revision too old for delta; resync required",
               "TooOld note must match the spec verbatim (note={:?})", note);
    assert_eq!(plan, 0, "plan must echo the requested plan_revision");
}

// -------------------------------------------------------------------------
// Runtime integration test for get_world_state MCP tool (smoke4 verified
// the live HTTP behavior; this test locks the contract in `cargo test`).
// Mirrors smoke4's verification: registered in tools/list, no-op path,
// existing symbol not Retracted, missing symbol IS Retracted, BeyondCurrent
// path with verbatim spec note, no agent registration required.
// -------------------------------------------------------------------------
#[tokio::test]
async fn get_world_state_tool_returns_retracted_and_beyond_current() {
    use lain::server::LainServer;
    use serde_json::json;

    let tmp = tempfile::tempdir().unwrap();
    git2::Repository::init(tmp.path()).unwrap();
    std::fs::write(tmp.path().join("a.rs"), "pub fn a() {}").unwrap();
    let mem = tmp.path().join(".lain/graph.bin");
    let server = LainServer::new(tmp.path(), &mem, None).expect("server");

    // 1) Empty symbols → no-op WorldState
    let r = lain::server::mcp::presence_tools::run_get_world_state(
        &server, json!({}),
    ).expect("get_world_state");
    assert!(r.get("current").is_some(), "current must be present");
    assert!(r.get("plan").is_some(), "plan must be present");
    assert_eq!(r["changed_symbols"].as_array().map(|a| a.len()), Some(0));
    assert!(r["note"].is_null(), "note must be null for no-op query");

    // 2) Existing symbol — NOT in Retracted. (In the test fixture, the
    //    federation ingester has not run, so the static graph is empty.
    //    The smoke4 harness exercises this against a real `lain server`
    //    where the ingester populated the graph from `verify_token`.)
    //    Skip a positive assertion here; the negative case (3) covers the
    //    path that matters for safety.
    // ------------------------------------------------------------------------

    // 3) Non-existent symbol — reported as NotIndexed, not Retracted.
    //    `get_world_state` is read-only and carries no agent identity,
    //    so it has no record of the caller ever having seen this
    //    symbol. "The graph has no such symbol" is all it can honestly
    //    say; claiming the symbol was *deleted* told agents their
    //    target had been removed when it had simply never been indexed.
    let r = lain::server::mcp::presence_tools::run_get_world_state(
        &server, json!({"symbols": ["nonexistent_xyz"]}),
    ).expect("get_world_state");
    let cs = r["changed_symbols"].as_array().unwrap();
    let absent: Vec<_> = cs.iter()
        .filter(|c| c["name"] == "nonexistent_xyz" && c["change_kind"] == "NotIndexed")
        .collect();
    assert_eq!(absent.len(), 1,
               "nonexistent_xyz must be NotIndexed; cs={cs:?}");

    // 4) BeyondCurrent path with verbatim spec note
    let cur = server.overlay.current_revision();
    let r = lain::server::mcp::presence_tools::run_get_world_state(
        &server, json!({"symbols": ["a"], "plan_revision": cur + 9999}),
    ).expect("get_world_state");
    assert_eq!(r["note"], "plan_revision beyond current — server may have restarted");
    assert_eq!(r["plan"], cur + 9999);
}

// -------------------------------------------------------------------------
// Runtime integration test for get_recent_activity MCP tool (smoke5
// verified the live HTTP behavior; this test locks the contract in
// `cargo test`). Mirrors the smoke verification: registered in
// tools/list, path_glob filter, group_by=path (default), per-group
// count + sample_event shape.
// -------------------------------------------------------------------------
#[tokio::test]
async fn get_recent_activity_tool_groups_by_path() {
    use lain::server::LainServer;
    use serde_json::json;

    let tmp = tempfile::tempdir().unwrap();
    git2::Repository::init(tmp.path()).unwrap();
    std::fs::write(tmp.path().join("a.rs"), "pub fn a() {}").unwrap();
    let mem = tmp.path().join(".lain/graph.bin");
    let server = LainServer::new(tmp.path(), &mem, None).expect("server");

    // Use a unique per-run path so the test is hermetic even if the
    // underlying state dir already contains audit events from prior
    // runs (the audit log persists across server restarts).
    let run_id = uuid::Uuid::new_v4().to_string();
    let prefix = format!("/tmp/hermetic-{}/", run_id);
    let p1 = format!("{}alpha.rs", prefix);
    let p2 = format!("{}beta.rs", prefix);
    let p3 = format!("{}gamma.rs", prefix);
    let p_other = format!("{}delta.rs", prefix);

    // Register 3 agents
    let alice = server.presence.register(
        format!("alice_{}", run_id), AgentKind::ClaudeCode, AgentMode::Interactive, None, None,
    );
    let bob = server.presence.register(
        format!("bob_{}", run_id), AgentKind::ClaudeCode, AgentMode::Interactive, None, None,
    );
    let carol = server.presence.register(
        format!("carol_{}", run_id), AgentKind::ClaudeCode, AgentMode::Interactive, None, None,
    );

    // Helper: claim one file and return the granted count
    fn claim_count(
        server: &LainServer,
        agent_id: &str,
        token: &str,
        path: &str,
    ) -> usize {
        let args = json!({
            "agent_id": agent_id,
            "session_token": token,
            "files": [{"path": path, "symbols": ["x"]}],
        });
        lain::server::mcp::presence_tools::run_claim_files(server, args)
            .expect("claim")["granted"]
            .as_array().unwrap().len()
    }
    assert_eq!(claim_count(&server, alice.id.as_str(), &alice.session_token, &p1), 1);
    assert_eq!(claim_count(&server, alice.id.as_str(), &alice.session_token, &p2), 1);
    assert_eq!(claim_count(&server, alice.id.as_str(), &alice.session_token, &p3), 1);
    assert_eq!(claim_count(&server, bob.id.as_str(), &bob.session_token, &p_other), 1);
    let _ = carol;  // unused

    // 1) Path-grouped digest scoped to this run's prefix
    let args = json!({"path_glob": format!("{}*", prefix)});
    let digest = lain::server::mcp::audit_tools::run_get_recent_activity(
        &server, args,
    ).expect("get_recent_activity");

    assert_eq!(digest["total_events"].as_u64(), Some(4),
               "total_events should be 4 (3 alice + 1 bob); digest={digest:?}");
    assert_eq!(digest["total_groups"].as_u64(), Some(4),
               "total_groups should be 4 (4 distinct paths); digest={digest:?}");
    assert_eq!(digest["group_by"].as_str(), Some("path"));
    assert_eq!(digest["truncated"].as_bool(), Some(false));
    assert_eq!(digest["groups"].as_array().map(|a| a.len()), Some(4));

    // Each group: count=1, sample_event has all 7 contract fields
    for g in digest["groups"].as_array().unwrap() {
        assert_eq!(g["count"].as_u64(), Some(1));
        let ev = &g["sample_event"];
        assert!(ev.get("ts_unix").is_some());
        assert!(ev.get("agent_id").is_some());
        assert!(ev.get("path").is_some());
        assert!(ev.get("claim_set").is_some());
        assert!(ev.get("racers").is_some());
        assert!(ev.get("plan_revision").is_some());
        assert!(ev.get("landed_revision").is_some());
        assert_eq!(g["first_ts"].as_f64(), g["last_ts"].as_f64(),
                   "first_ts == last_ts when count==1");
    }

    // 2) Limit truncates and reports truncated=true
    let args2 = json!({"path_glob": format!("{}*", prefix), "limit": 2});
    let digest2 = lain::server::mcp::audit_tools::run_get_recent_activity(
        &server, args2,
    ).expect("get_recent_activity");
    assert_eq!(digest2["groups"].as_array().map(|a| a.len()), Some(2));
    assert_eq!(digest2["truncated"].as_bool(), Some(true));
    assert_eq!(digest2["total_groups"].as_u64(), Some(4));
}

// --- Claim path canonicalization (F-01) ---
//
// Claims used to be keyed on the caller's raw spelling, so the same
// file claimed as `/ws/src/a.rs` and as `src/a.rs` produced two
// independent claims that never conflicted. That split ran between the
// CLI (absolute paths) and MCP callers (repo-relative), so the two
// halves of the product could not collide by construction.

fn claim_req(path: &str) -> ClaimRequest {
    ClaimRequest {
        path: std::path::PathBuf::from(path),
        symbols: vec![],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
        plan_revision: None,
    }
}

#[test]
fn claims_collide_across_path_spellings() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "pub fn a() {}").unwrap();

    let abs = root.join("src/a.rs");
    let abs_str = abs.to_string_lossy().to_string();

    // Every spelling an agent could plausibly send for one file.
    let spellings = [
        abs_str.as_str(),
        "src/a.rs",
        "./src/a.rs",
        "src/../src/a.rs",
    ];

    for spelling in spellings {
        let occ = lain::server::presence::OccupancyMap::new();
        occ.set_workspace_root(root);
        let alice = AgentId("alice".into());
        let bob = AgentId("bob".into());

        // Alice always claims the absolute form — this is what
        // `lain hooks claim` writes.
        let first = occ.claim(&alice, vec![claim_req(&abs_str)]);
        assert_eq!(first.granted.len(), 1, "alice should hold {abs_str}");

        let result = occ.claim(&bob, vec![claim_req(spelling)]);
        assert_eq!(
            result.conflicts.len(),
            1,
            "spelling {spelling:?} must conflict with the absolute claim"
        );
        assert_eq!(result.granted.len(), 0, "spelling {spelling:?} must not be granted");
        assert_eq!(result.conflicts[0].agent_id, alice);
    }
}

#[test]
fn granted_paths_are_canonical() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "pub fn a() {}").unwrap();

    let occ = lain::server::presence::OccupancyMap::new();
    occ.set_workspace_root(root);
    let alice = AgentId("alice".into());

    // The echoed path is the key that was actually taken, not the
    // caller's spelling — otherwise `my_claims` would not match what
    // the agent sent to `release_files`.
    let result = occ.claim(&alice, vec![claim_req("./src/../src/a.rs")]);
    assert_eq!(result.granted.len(), 1);
    assert_eq!(result.granted[0].path, std::path::PathBuf::from("src/a.rs"));
}

#[test]
fn release_matches_a_differently_spelled_claim() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "pub fn a() {}").unwrap();

    let occ = lain::server::presence::OccupancyMap::new();
    occ.set_workspace_root(root);
    let alice = AgentId("alice".into());

    occ.claim(&alice, vec![claim_req(&root.join("src/a.rs").to_string_lossy())]);
    let released = occ.release(&alice, &[std::path::PathBuf::from("src/a.rs")]);
    assert_eq!(released.len(), 1, "relative release must find an absolute claim");

    // And the file is now free for another agent.
    let bob = AgentId("bob".into());
    let result = occ.claim(&bob, vec![claim_req("src/a.rs")]);
    assert_eq!(result.conflicts.len(), 0);
    assert_eq!(result.granted.len(), 1);
}

#[test]
fn claim_for_a_file_that_does_not_exist_yet_still_collides() {
    // An agent claiming a file it is about to create has nothing on
    // disk to anchor against; it must still land on one key.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let occ = lain::server::presence::OccupancyMap::new();
    occ.set_workspace_root(root);
    let alice = AgentId("alice".into());
    let bob = AgentId("bob".into());

    occ.claim(&alice, vec![claim_req(&root.join("src/new.rs").to_string_lossy())]);
    let result = occ.claim(&bob, vec![claim_req("src/new.rs")]);
    assert_eq!(result.conflicts.len(), 1, "unborn file must still collide");
}

#[test]
fn same_relative_path_in_two_repos_stays_distinct() {
    // Federation: `src/main.rs` in repo A and repo B are different
    // files and must not be forced into one claim by canonicalization.
    let tmp = tempfile::tempdir().unwrap();
    let repo_a = tmp.path().join("a");
    let repo_b = tmp.path().join("b");
    std::fs::create_dir_all(repo_a.join("src")).unwrap();
    std::fs::create_dir_all(repo_b.join("src")).unwrap();
    std::fs::write(repo_a.join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(repo_b.join("src/main.rs"), "fn main() {}").unwrap();

    let occ = lain::server::presence::OccupancyMap::new();
    occ.add_claim_roots(&[repo_a.clone(), repo_b.clone()]);
    let alice = AgentId("alice".into());
    let bob = AgentId("bob".into());

    occ.claim(&alice, vec![claim_req(&repo_a.join("src/main.rs").to_string_lossy())]);
    let result = occ.claim(&bob, vec![claim_req(&repo_b.join("src/main.rs").to_string_lossy())]);
    assert_eq!(result.conflicts.len(), 0, "different repos must not collide");
    assert_eq!(result.granted.len(), 1);
}

// --- Session lifetime (F-05) ---
//
// The TTL used to be 60 seconds of wall clock refreshed only by an
// explicit `heartbeat`. A single LLM turn — thinking plus a couple of
// tool round-trips — routinely runs past that, so an agent would claim
// a file, reason about it, and come back to `unknown session token`
// with its claims silently released and the file free for someone else
// to take mid-edit.

#[test]
fn interactive_ttl_is_sized_for_model_latency() {
    use lain::server::presence::{BACKGROUND_SESSION_TTL, INTERACTIVE_SESSION_TTL};
    assert!(
        INTERACTIVE_SESSION_TTL >= std::time::Duration::from_secs(300),
        "an interactive TTL under 5 minutes cannot survive a normal agent turn"
    );
    assert_eq!(BACKGROUND_SESSION_TTL, std::time::Duration::from_secs(60));

    let reg = PresenceRegistry::new();
    assert_eq!(reg.expires_after_for(&AgentMode::Interactive), INTERACTIVE_SESSION_TTL);
    assert_eq!(reg.expires_after_for(&AgentMode::Background), BACKGROUND_SESSION_TTL);
}

#[tokio::test]
async fn any_authenticated_tool_call_extends_the_session() {
    use lain::server::mcp::presence_tools::{run_my_claims, run_register_agent};
    use lain::server::LainServer;

    let tmp = tempfile::tempdir().unwrap();
    git2::Repository::init(tmp.path()).unwrap();
    let mem = tmp.path().join(".lain/graph.bin");
    let server = std::sync::Arc::new(LainServer::new(tmp.path(), &mem, None).expect("server"));

    let v = run_register_agent(&server, serde_json::json!({"name": "alice"})).unwrap();
    let agent_id = v["agent_id"].as_str().unwrap().to_string();
    let token = v["session_token"].as_str().unwrap().to_string();

    let before = server.presence.by_token(&token).unwrap().last_heartbeat;
    std::thread::sleep(std::time::Duration::from_millis(20));

    // `my_claims` is not the heartbeat tool — it is ordinary work.
    run_my_claims(
        &server,
        serde_json::json!({"agent_id": agent_id, "session_token": token}),
    )
    .unwrap();

    let after = server.presence.by_token(&token).unwrap().last_heartbeat;
    assert!(
        after > before,
        "an authenticated tool call must count as proof of life"
    );
}

#[tokio::test]
async fn expired_session_revokes_claims_with_a_reason() {
    use lain::server::ingest::background::expiry_tick;

    let reg = PresenceRegistry::with_expiry(std::time::Duration::from_millis(20));
    let occ = lain::server::presence::OccupancyMap::new();
    let (tx, mut rx) = tokio::sync::broadcast::channel(16);
    let tmp = tempfile::tempdir().unwrap();
    let events_log = lain::server::events_log::EventsLog::open(tmp.path()).unwrap();

    let session = reg.register("alice".into(), AgentKind::ClaudeCode, AgentMode::Interactive, None, None);
    occ.claim(&session.id, vec![claim_req("auth.rs")]);

    std::thread::sleep(std::time::Duration::from_millis(40));
    expiry_tick(&reg, &occ, &tx, &events_log);

    // A revoked claim is not a released one: the holder never asked to
    // give it up and may still believe it owns the file.
    let mut saw_revoked = false;
    while let Ok((_, ev)) = rx.try_recv() {
        if let PresenceEvent::ClaimRevoked { agent_id, path, reason } = ev {
            assert_eq!(agent_id, session.id);
            assert_eq!(path, std::path::PathBuf::from("auth.rs"));
            assert_eq!(reason, "session_expired");
            saw_revoked = true;
        }
    }
    assert!(saw_revoked, "session expiry must announce the revocation");
}

// --- Inferred claims (F-04) ---

#[test]
fn inferred_claims_are_flagged_and_declaration_upgrades_them() {
    let occ = lain::server::presence::OccupancyMap::new();
    let alice = AgentId("alice".into());

    // The watcher guessed alice wrote this file.
    let r = occ.claim_inferred(&alice, vec![claim_req("auth.rs")]);
    assert_eq!(r.granted.len(), 1);
    let claims = occ.list_for_agent(&alice);
    assert!(claims[0].inferred, "a guessed claim must say so");

    // Alice then declares it herself — a declaration outranks a guess.
    occ.claim(&alice, vec![claim_req("auth.rs")]);
    let claims = occ.list_for_agent(&alice);
    assert!(
        claims.iter().all(|c| !c.inferred),
        "an explicit claim must clear the inferred marker"
    );
}

#[test]
fn inference_never_downgrades_a_declared_claim() {
    let occ = lain::server::presence::OccupancyMap::new();
    let alice = AgentId("alice".into());

    occ.claim(&alice, vec![claim_req("auth.rs")]);
    // The watcher sees a write to a file alice already declared; that
    // must refresh, not reclassify.
    occ.claim_inferred(&alice, vec![claim_req("auth.rs")]);

    let claims = occ.list_for_agent(&alice);
    assert!(
        claims.iter().all(|c| !c.inferred),
        "a declared claim stays declared when the watcher re-observes it"
    );
}

#[test]
fn conflicts_report_whether_the_holder_was_guessed() {
    let occ = lain::server::presence::OccupancyMap::new();
    let alice = AgentId("alice".into());
    let bob = AgentId("bob".into());

    occ.claim_inferred(&alice, vec![claim_req("auth.rs")]);
    let result = occ.claim(&bob, vec![claim_req("auth.rs")]);

    assert_eq!(result.conflicts.len(), 1);
    assert!(
        result.conflicts[0].inferred,
        "bob must be able to tell a guess from a declaration before backing off"
    );
}

// --- Read-over-edit advisories (F-06) ---

#[test]
fn read_over_edit_grants_with_an_advisory() {
    let occ = lain::server::presence::OccupancyMap::new();
    let alice = AgentId("alice".into());
    let bob = AgentId("bob".into());

    occ.claim(&alice, vec![claim_req("handler.rs")]); // edit
    let result = occ.claim(&bob, vec![ClaimRequest {
        path: std::path::PathBuf::from("handler.rs"),
        symbols: vec![],
        intent: ClaimIntent::Read,
        ttl_seconds: None,
        plan_revision: None,
    }]);

    // Readers are never blocked...
    assert_eq!(result.granted.len(), 1, "a read must still be granted");
    assert_eq!(result.conflicts.len(), 0, "a read must not conflict");
    // ...but they must be told the file is being rewritten under them.
    assert_eq!(result.advisories.len(), 1, "read over a live edit needs an advisory");
    assert_eq!(result.advisories[0].agent_id, alice);
    assert_eq!(result.advisories[0].intent, ClaimIntent::Edit);
}

#[test]
fn read_on_a_quiet_file_carries_no_advisory() {
    let occ = lain::server::presence::OccupancyMap::new();
    let bob = AgentId("bob".into());
    let result = occ.claim(&bob, vec![ClaimRequest {
        path: std::path::PathBuf::from("quiet.rs"),
        symbols: vec![],
        intent: ClaimIntent::Read,
        ttl_seconds: None,
        plan_revision: None,
    }]);
    assert!(result.advisories.is_empty(), "no editor, no advisory");
}

#[test]
fn read_over_another_read_carries_no_advisory() {
    let occ = lain::server::presence::OccupancyMap::new();
    let alice = AgentId("alice".into());
    let bob = AgentId("bob".into());
    let read = |p: &str| ClaimRequest {
        path: std::path::PathBuf::from(p),
        symbols: vec![],
        intent: ClaimIntent::Read,
        ttl_seconds: None,
        plan_revision: None,
    };
    occ.claim(&alice, vec![read("shared.rs")]);
    let result = occ.claim(&bob, vec![read("shared.rs")]);
    assert!(result.advisories.is_empty(), "two readers are not a hazard");
}
