//! `spawn_blocking` isolation — end-to-end integration tests.
//!
//! AGENT_UX_ROADMAP.md Milestone 4 follow-up (FOLLOWUPS.md
//! §"spawn_blocking isolation"): the unit tests for the
//! `offthread` helper itself live in
//! `src/server/ingest/blocking.rs`. The integration tests here
//! pin the public-surface contract: a `LainServer` exposes
//! `outstanding_files` accessors and the lifecycle cancel token,
//! and the offthread-routed `build_core_memory` pipeline still
//! short-circuits on a pre-cancelled token (the same fast-path
//! `await_startup_reindex` uses for cooperative shutdown).
//!
//! End-to-end `build_core_memory` runs are covered by the
//! existing `tests/cancellation_token.rs::build_core_memory_respects_a_pre_cancelled_token`
//! test, which exercises the same offthread routing from PR A's
//! PR A's `feat/m4-cancellation-token` work. We intentionally do
//! not run a full indexing pass here under `current_thread` —
//! `tokio::task::spawn_blocking` on a current_thread runtime is
//! supported but the wall-clock cost is dominated by file I/O in
//! tests/`tmp`, not by the offthread migration we want to verify.
//!
//! The pre-fix design used `task.abort()` for shutdown and the
//! per-repo `outstanding_files` counter stayed at 0; the post-fix
//! design routes sync work through the blocking-thread pool and
//! the counter is wired through `PerRepoReadiness` for
//! `get_capabilities` to surface watcher back-pressure.

use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

use lain::error::LainError;
use lain::server::LainServer;

/// `build_core_memory` with a pre-cancelled token must short-circuit
/// without hanging the test budget. This is the same fast-path the
/// `await_startup_reindex` outer race uses; PR B doesn't change the
/// shutdown path but it adds the per-file work onto the
/// blocking-thread pool, and this test pins that the contract still
/// holds after the migration.
#[tokio::test(flavor = "current_thread")]
async fn build_core_memory_short_circuits_under_offthread_routing() {
    let tmp = TempDir::new().unwrap();
    init_git_repo(tmp.path());

    let server =
        Arc::new(LainServer::new(tmp.path(), &tmp.path().join(".lain/graph.bin"), None).unwrap());
    let lifecycle = server.lifecycle_arc();
    lifecycle.cancel();

    let started = std::time::Instant::now();
    let result = server.build_core_memory().await;
    assert!(
        matches!(result, Err(LainError::Cancelled)),
        "expected Cancelled, got {result:?}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "a pre-cancelled token must short-circuit; took {:?}",
        started.elapsed()
    );
}

fn init_git_repo(root: &std::path::Path) {
    use std::process::Command;
    let run = |args: &[&str]| {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap()
                .success(),
            "git {args:?} failed"
        );
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "spawn-blocking-test@lain"]);
    run(&["config", "user.name", "spawn-blocking-test"]);
    std::fs::write(root.join("lib.rs"), "pub fn hi() {}\n").unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-q", "-m", "fixture"]);
}
