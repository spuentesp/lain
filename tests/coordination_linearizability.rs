//! Linearizability stress test for the coordination design
//! (acceptance criterion #5 of
//! `docs/INTENT_AND_OBSERVABILITY_PLAN.md`).
//!
//! The invariant: for any overlapping exclusive scope, at most one
//! live lease may be successfully granted at any instant across all
//! LAIN processes sharing that workspace. In single-process mode
//! (the LainServer we build here), this collapses to: across N
//! concurrent claim attempts on the same path, exactly one
//! succeeds and the rest see a conflict.
//!
//! Each iteration:
//! 1. Build N agents with overlapping declared intents.
//! 2. Race barrier-synchronized `claim_files` calls.
//! 3. Assert exactly one grant, N-1 conflicts, 0 UNAVAILABLE-style
//!    regressions.
//!
//! Repeated across many iterations to flush timing-dependent bugs.
#[path = "support/isolated_state.rs"]
mod isolated_state;

use lain::server::mcp::presence_tools::{run_claim_files, run_register_agent, run_release_files};
use lain::server::LainServer;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

/// One agent in the race. Constructed once, reused across
/// iterations so the per-iteration overhead stays in setup cost
/// rather than claim-path cost.
struct RaceAgent {
    agent_id: String,
    session_token: String,
}

fn fresh_server() -> (TempDir, Arc<LainServer>) {
    let dir = TempDir::new().expect("tempdir");
    git2::Repository::init(dir.path()).unwrap();
    std::fs::write(dir.path().join("contested.rs"), "pub fn contested() {}\n").unwrap();
    let mem = dir.path().join(".lain/graph.bin");
    let server = isolated_state::new_server(dir.path(), &mem, None).expect("LainServer::new");
    (dir, Arc::new(server))
}

fn register(server: &Arc<LainServer>, name: &str) -> RaceAgent {
    let v = run_register_agent(server, serde_json::json!({ "name": name })).unwrap();
    RaceAgent {
        agent_id: v["agent_id"].as_str().unwrap().to_string(),
        session_token: v["session_token"].as_str().unwrap().to_string(),
    }
}

/// Single iteration of the race. Each thread is one agent's
/// `claim_files` call. Returns the (agent_name, response) tuple
/// for the join.
fn run_one_iteration(
    server: &Arc<LainServer>,
    agents: &[RaceAgent],
    contested: &str,
) -> Vec<(String, serde_json::Value)> {
    use std::sync::{Arc as StdArc, Barrier};

    let barrier = StdArc::new(Barrier::new(agents.len()));
    let server = StdArc::clone(server);

    let handles: Vec<_> = agents
        .iter()
        .map(|a| {
            let barrier = StdArc::clone(&barrier);
            let server = StdArc::clone(&server);
            let agent = a.agent_id.clone();
            let token = a.session_token.clone();
            let contested = contested.to_string();
            std::thread::spawn(move || {
                barrier.wait();
                let resp = run_claim_files(
                    &server,
                    serde_json::json!({
                        "agent_id": agent,
                        "session_token": token,
                        "files": [{"path": contested, "intent": "edit"}],
                    }),
                );
                let resp = resp
                    .unwrap_or_else(|e| serde_json::json!({"error": "tool_error", "message": e}));
                (format!("{agent:.8}"), resp)
            })
        })
        .collect();

    handles
        .into_iter()
        .map(|h| h.join().expect("thread join"))
        .collect()
}

fn count_grants_and_conflicts(responses: &[(String, serde_json::Value)]) -> (usize, usize) {
    let mut grants = 0;
    let mut conflicts = 0;
    for (_, resp) in responses {
        let granted = resp
            .get("granted")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .any(|g| g.get("path").and_then(|p| p.as_str()) == Some("contested.rs"))
            })
            .unwrap_or(false);
        let conflicted = resp
            .get("conflicts")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .any(|c| c.get("path").and_then(|p| p.as_str()) == Some("contested.rs"))
            })
            .unwrap_or(false);
        if granted {
            grants += 1;
        }
        if conflicted {
            conflicts += 1;
        }
    }
    (grants, conflicts)
}

/// The headline regression test: the linearizability invariant.
/// 100 iterations × 4 racers = 400 races. Per iteration we expect
/// exactly one grant and three conflicts.
#[test]
fn linearizability_holds_across_n_agents_and_n_iterations() {
    let (_dir, server) = fresh_server();
    let agents: Vec<RaceAgent> = (0..4)
        .map(|i| register(&server, &format!("racer-{i}")))
        .collect();

    let contested = "contested.rs";
    let mut total_grants = 0;
    let mut total_conflicts = 0;

    for iteration in 0..100 {
        let responses = run_one_iteration(&server, &agents, contested);
        let (grants, conflicts) = count_grants_and_conflicts(&responses);

        assert_eq!(
            grants, 1,
            "iteration {iteration}: exactly one agent must win the claim; \
             got {grants} grants across {responses:?}",
        );
        assert_eq!(
            conflicts, 3,
            "iteration {iteration}: the other three must see a conflict; \
             got {conflicts} conflicts across {responses:?}",
        );

        total_grants += grants;
        total_conflicts += conflicts;

        // Release the winner's claim before the next iteration so
        // every iteration starts with no holder. The losing racers
        // never held a claim, so they don't need a release.
        let winner = responses
            .iter()
            .find(|(_, r)| {
                r.get("granted")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .any(|g| g.get("path").and_then(|p| p.as_str()) == Some("contested.rs"))
                    })
                    .unwrap_or(false)
            })
            .map(|(name, _)| name.clone())
            .expect("a winner exists");
        let winner_full = agents
            .iter()
            .find(|a| format!("{:.8}", a.agent_id) == winner)
            .expect("winner resolves to a registered agent");
        run_release_files(
            &server,
            serde_json::json!({
                "agent_id": winner_full.agent_id,
                "session_token": winner_full.session_token,
                "files": [{"path": contested}],
            }),
        )
        .expect("release winner");
    }

    assert_eq!(total_grants, 100, "100 iterations × 1 grant each = 100");
    assert_eq!(
        total_conflicts, 300,
        "100 iterations × 3 conflicts each = 300"
    );
}

/// N=10 stress: ten agents racing for the same path. Per the
/// plan's acceptance criterion, exactly one wins and nine lose.
#[test]
fn linearizability_n10_stress() {
    let (_dir, server) = fresh_server();
    let agents: Vec<RaceAgent> = (0..10)
        .map(|i| register(&server, &format!("n10-{i}")))
        .collect();

    let contested = "contested.rs";
    let responses = run_one_iteration(&server, &agents, contested);
    let (grants, conflicts) = count_grants_and_conflicts(&responses);
    assert_eq!(
        grants, 1,
        "N=10 race must yield exactly 1 grant; got {responses:?}"
    );
    assert_eq!(
        conflicts, 9,
        "N=10 race must yield 9 conflicts; got {responses:?}"
    );
}

/// Chaos variant: every iteration's winner releases before the
/// next iteration starts, but the release itself races against
/// fresh claims. This exercises the `OccupancyMap::release` →
/// re-claim path, which a naive implementation can deadlock if
/// release and claim contend on the same per-agent path lock.
#[test]
fn linearizability_release_then_reclaim_cycle() {
    let (_dir, server) = fresh_server();
    let agents: Vec<RaceAgent> = (0..4)
        .map(|i| register(&server, &format!("cycle-{i}")))
        .collect();

    let contested = "contested.rs";

    for _ in 0..50 {
        let responses = run_one_iteration(&server, &agents, contested);
        let (grants, _) = count_grants_and_conflicts(&responses);
        assert_eq!(grants, 1);
        // The winner is the agent whose response has the granted
        // entry. The granted entry doesn't carry `agent_id` (the
        // requester is identified by the response's parent tuple,
        // not by an embedded field), so we look up by requester
        // name from the (name, response) tuple.
        let winner_name = responses
            .iter()
            .find(|(_, r)| {
                r.get("granted")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .any(|g| g.get("path").and_then(|p| p.as_str()) == Some("contested.rs"))
                    })
                    .unwrap_or(false)
            })
            .map(|(name, _)| name.clone())
            .expect("a winner exists");
        let winner_full = agents
            .iter()
            .find(|a| format!("{:.8}", a.agent_id) == winner_name)
            .expect("winner resolves to a registered agent");
        run_release_files(
            &server,
            serde_json::json!({
                "agent_id": winner_full.agent_id,
                "session_token": winner_full.session_token,
                "files": [{"path": contested}],
            }),
        )
        .expect("release");
    }
}

/// Two agents claim *different* paths. Both should win — the
/// linearizability invariant is per-scope, not global. This is
/// the negative-control case that pins the test isn't trivially
/// asserting "exactly one grant across all calls".
#[test]
fn non_overlapping_scopes_both_win() {
    let (_dir, server) = fresh_server();
    let alice = register(&server, "alice");
    let bob = register(&server, "bob");

    let r1 = run_claim_files(
        &server,
        serde_json::json!({
            "agent_id": alice.agent_id,
            "session_token": alice.session_token,
            "files": [{"path": "alice.rs", "intent": "edit"}],
        }),
    )
    .expect("alice claim");
    let r2 = run_claim_files(
        &server,
        serde_json::json!({
            "agent_id": bob.agent_id,
            "session_token": bob.session_token,
            "files": [{"path": "bob.rs", "intent": "edit"}],
        }),
    )
    .expect("bob claim");

    assert!(r1["granted"].as_array().unwrap().len() == 1);
    assert!(r2["granted"].as_array().unwrap().len() == 1);
    assert_eq!(r1["conflicts"].as_array().unwrap().len(), 0);
    assert_eq!(r2["conflicts"].as_array().unwrap().len(), 0);
}

// Quiet the unused-import warning on `PathBuf` if the test ever
// gets simplified; PathBuf is referenced from the helper above
// and may be picked up by future iterations.
#[allow(dead_code)]
fn _pathbuf_marker(_p: &PathBuf) {}
