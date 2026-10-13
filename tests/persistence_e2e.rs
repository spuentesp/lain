//! Session-registry persistence across a lain restart.
//!
//! Claims no longer round-trip through lain's state file — they live
//! in zoanthrope's `.zoanthrope/state.json` and survive by its own
//! persistence. What must survive a lain restart is lain's own state:
//! the session auth table (agents + `session_token`s) and the activity
//! feed. This test pins that.

use lain::server::activity::ActivityTracker;
use lain::server::ingest::LainServer;
use lain::server::presence::{load_pair, save_pair, AgentKind, AgentMode};
use std::path::PathBuf;
use tempfile::tempdir;

#[test]
fn sessions_survive_lain_restart() {
    let tmp = tempdir().unwrap();
    let stem = "restart-test";
    let state_path: PathBuf = tmp.path().join(format!("{stem}.json"));
    let mem = tmp.path().join("graph.bin");

    // `LainServer::new` -> `GitSensor::new` calls `git2::Repository::open`,
    // which requires an initialized repo. Same precondition the other
    // LainServer-based tests use.
    git2::Repository::init(tmp.path()).unwrap();

    // Round 1: register one agent, persist the session table.
    let server1 = LainServer::new(tmp.path(), &mem, None).expect("server");
    let agent = server1.presence().register(
        "alice".into(),
        AgentKind::ClaudeCode,
        AgentMode::Interactive,
        Some(99999),
        None,
    );
    save_pair(&state_path, server1.presence(), &ActivityTracker::new()).expect("save");

    // Drop server1.
    drop(server1);

    // Round 2: new server, same data dir.
    let server2 = LainServer::new(tmp.path(), &mem, None).expect("server");
    load_pair(&state_path, server2.presence(), &ActivityTracker::new()).expect("load");

    let active = server2.presence().list_active(true);
    assert_eq!(active.len(), 1, "agent should survive restart");
    assert_eq!(active[0].name, "alice");

    // The session token stays valid across the restart — that is the
    // auth contract hooks depend on.
    let by_token = server2.presence().by_token(&agent.session_token);
    assert!(
        by_token.is_some(),
        "session token must resolve after restart"
    );
}
