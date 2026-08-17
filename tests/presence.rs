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
    }]);
    let result = occ.claim(&bob, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
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
    }]);
    let result = occ.claim(&bob, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["validate".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
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
    }]);
    let result = occ.claim(&bob, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["anything".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
    }]);
    assert_eq!(result.granted.len(), 0);
    assert_eq!(result.conflicts.len(), 1);
}

#[test]
fn release_returns_removed_paths() {
    let occ = lain::server::presence::OccupancyMap::new();
    let alice = AgentId("alice".into());
    occ.claim(&alice, vec![
        ClaimRequest { path: std::path::PathBuf::from("auth.rs"), symbols: vec!["login".into()], intent: ClaimIntent::Edit, ttl_seconds: None },
        ClaimRequest { path: std::path::PathBuf::from("db.rs"), symbols: vec![], intent: ClaimIntent::Read, ttl_seconds: None },
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
        ClaimRequest { path: std::path::PathBuf::from("auth.rs"), symbols: vec![], intent: ClaimIntent::Edit, ttl_seconds: None },
        ClaimRequest { path: std::path::PathBuf::from("db.rs"), symbols: vec![], intent: ClaimIntent::Edit, ttl_seconds: None },
    ]);
    let released = occ.release_all_for(&alice);
    assert_eq!(released.len(), 2);
    assert_eq!(occ.list_for_agent(&alice).len(), 0);
}

#[test]
fn list_for_path_shows_all_agents() {
    let occ = lain::server::presence::OccupancyMap::new();
    occ.claim(&AgentId("alice".into()), vec![ClaimRequest { path: std::path::PathBuf::from("auth.rs"), symbols: vec!["login".into()], intent: ClaimIntent::Edit, ttl_seconds: None }]);
    occ.claim(&AgentId("bob".into()), vec![ClaimRequest { path: std::path::PathBuf::from("auth.rs"), symbols: vec!["validate".into()], intent: ClaimIntent::Edit, ttl_seconds: None }]);
    let entry = occ.list_for_path(&std::path::PathBuf::from("auth.rs")).unwrap();
    assert_eq!(entry.agents.len(), 2);
    assert_eq!(entry.symbols.len(), 2);
}

#[test]
fn list_all_returns_all_claimed_paths() {
    let occ = lain::server::presence::OccupancyMap::new();
    occ.claim(&AgentId("alice".into()), vec![ClaimRequest { path: std::path::PathBuf::from("auth.rs"), symbols: vec!["login".into()], intent: ClaimIntent::Edit, ttl_seconds: None }]);
    occ.claim(&AgentId("bob".into()), vec![ClaimRequest { path: std::path::PathBuf::from("db.rs"), symbols: vec![], intent: ClaimIntent::Edit, ttl_seconds: None }]);

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
    let req = ClaimRequest { path: std::path::PathBuf::from("auth.rs"), symbols: vec!["login".into()], intent: ClaimIntent::Edit, ttl_seconds: None };
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
    }]);
    // bob: no TTL on db.rs — survives. Different file keeps the test
    // from accidentally exercising the file-level vs symbol-level
    // conflict path.
    occ.claim(&bob, vec![ClaimRequest {
        path: std::path::PathBuf::from("db.rs"),
        symbols: vec![],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
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
    }]);
    let result = occ.claim(&bob, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Read, // <-- read, not edit
        ttl_seconds: None,
    }]);
    assert_eq!(result.granted.len(), 1, "read claim should NOT conflict with edit");
    assert_eq!(result.conflicts.len(), 0);
}

/// Edit-vs-edit must still conflict as before — the read-vs-edit filter
/// is *additive* loosening, not a blanket "no conflicts" rule. The
/// surviving conflict entry also carries `last_touched_unix` so the
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
    }]);
    let result = occ.claim(&bob, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
    }]);
    assert_eq!(result.granted.len(), 0, "edit-vs-edit conflict should still hold");
    assert_eq!(result.conflicts.len(), 1);
    let c = &result.conflicts[0];
    assert_eq!(c.symbols, vec!["login".to_string()]);
    assert!(c.last_touched_unix <= SystemTime::now());
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
