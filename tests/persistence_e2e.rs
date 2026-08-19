use lain::server::ingest::LainServer;
use lain::server::presence::{save_pair, load_pair, AgentKind, AgentMode, ClaimIntent, ClaimRequest};
use std::path::PathBuf;
use tempfile::tempdir;

#[test]
fn presence_and_occupancy_survive_lain_restart() {
    let tmp = tempdir().unwrap();
    let stem = "restart-test";
    let state_path: PathBuf = tmp.path().join(format!("{stem}.json"));
    let mem = tmp.path().join("graph.bin");

    // `LainServer::new` -> `GitSensor::new` calls `git2::Repository::open`,
    // which requires an initialized repo. Same precondition the other
    // LainServer-based tests in `presence.rs` use.
    git2::Repository::init(tmp.path()).unwrap();

    // Round 1: server with empty data dir.
    let server1 = LainServer::new(tmp.path(), &mem, None).expect("server");
    let agent = server1.presence.register(
        "alice".into(),
        AgentKind::ClaudeCode,
        AgentMode::Interactive,
        Some(99999),
        None,
    );
    server1.occupancy.claim(&agent.id, vec![ClaimRequest {
        path: tmp.path().join("foo.rs"),
        symbols: vec!["bar".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
        plan_revision: None,
    }]);
    save_pair(&state_path, &server1.presence, &server1.occupancy).expect("save");

    // Drop server1.
    drop(server1);

    // Round 2: new server, same data dir.
    let server2 = LainServer::new(tmp.path(), &mem, None).expect("server");
    load_pair(&state_path, &server2.presence, &server2.occupancy).expect("load");

    let active = server2.presence.list_active(true);
    assert_eq!(active.len(), 1, "agent should survive restart");
    assert_eq!(active[0].name, "alice");

    let claims = server2.occupancy.list_for_agent(&agent.id);
    assert_eq!(claims.len(), 1, "claim should survive restart");
}
