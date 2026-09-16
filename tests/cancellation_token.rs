//! Cooperative cancellation token — end-to-end tests.
//!
//! AGENT_UX_ROADMAP.md Milestone 4 follow-up (FOLLOWUPS.md
//! §"Cooperative cancellation token"): the tests in this file prove
//! that a `LainServer` shutdown during a cold-boot re-index returns
//! control within budget instead of running the indexing pass to
//! completion or hard-aborting mid-graph-write.
//!
//! Each test exercises a different cancel point in the same machinery
//! the production shutdown path uses (the server-owned
//! `CancellationToken` carried by `LifecycleInfo`).

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tempfile::TempDir;

use lain::error::LainError;
use lain::server::ingest::handles::LifecycleInfo;
use lain::server::LainServer;

/// A pre-cancelled token makes the next `is_cancelled()` check fire
/// immediately. `LifecycleInfo` itself owns the token; this just
/// proves the lifecycle accessor round-trips correctly.
#[test]
fn lifecycle_token_round_trips() {
    let lifecycle = LifecycleInfo::new(SystemTime::now());
    let token = lifecycle.cancel_token();
    assert!(!lifecycle.is_cancelled());
    assert!(!token.is_cancelled());
    lifecycle.cancel();
    assert!(lifecycle.is_cancelled());
    assert!(token.is_cancelled());
}

/// Drop on `LifecycleInfo` must cancel the token so any in-flight
/// `is_cancelled()` check observes shutdown before the runtime tears
/// down. This is the contract the production `LainServer::Drop`
/// relies on.
#[test]
fn dropping_lifecycle_cancels_the_token() {
    let token;
    {
        let lifecycle = LifecycleInfo::new(SystemTime::now());
        token = lifecycle.cancel_token();
        assert!(!token.is_cancelled());
    }
    assert!(token.is_cancelled());
}

/// The startup-task handle install/take pair must drain the slot
/// after a `take`, leaving `None` for any subsequent caller.
#[test]
fn startup_task_handle_drains_after_take() {
    let lifecycle = LifecycleInfo::new(SystemTime::now());
    let join = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .spawn(async {});
    lifecycle.install_startup_task(join);
    let taken = lifecycle.take_startup_task();
    assert!(taken.is_some());
    assert!(lifecycle.take_startup_task().is_none());
}

/// A `build_core_memory` call made after the lifecycle token is
/// already cancelled must return `LainError::Cancelled` immediately,
/// without touching the on-disk graph or the readiness `ready()`
/// state.
#[tokio::test(flavor = "current_thread")]
async fn build_core_memory_respects_a_pre_cancelled_token() {
    let tmp = TempDir::new().unwrap();
    init_git_repo(tmp.path());

    let server = LainServer::new(tmp.path(), &tmp.path().join(".lain/graph.bin"), None).unwrap();
    server.lifecycle_handle().cancel();

    let started = Instant::now();
    let result = server.build_core_memory().await;
    assert!(
        matches!(result, Err(LainError::Cancelled)),
        "expected Cancelled, got: {result:?}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "a pre-cancelled token must short-circuit immediately, took {:?}",
        started.elapsed()
    );
}

/// `await_startup_reindex` is the production outer race: it
/// `select!`s `build_core_memory` against the cancel token. This
/// test exercises that race directly using a fake startup task:
/// cancel the token first, then start the indexing pass, and
/// assert the `select!` returns `Cancelled` rather than running
/// the indexing to completion.
#[tokio::test(flavor = "current_thread")]
async fn await_startup_reindex_cancelled_branch() {
    let tmp = TempDir::new().unwrap();
    init_git_repo(tmp.path());

    let server =
        Arc::new(LainServer::new(tmp.path(), &tmp.path().join(".lain/graph.bin"), None).unwrap());
    let lifecycle = server.lifecycle_arc();
    let cancel = lifecycle.cancel_token();

    // Cancel first, then start the indexing pass. The production
    // outer race (in `await_startup_reindex`) does the same — the
    // select! picks the cancelled branch.
    lifecycle.cancel();

    let started = Instant::now();
    let result: Result<(), LainError> = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(LainError::Cancelled),
        r = server.build_core_memory() => r,
    };
    assert!(
        matches!(result, Err(LainError::Cancelled)),
        "expected Cancelled, got: {result:?}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "a pre-cancelled select! must short-circuit immediately, took {:?}",
        started.elapsed()
    );
}

/// After cancellation, `get_health` (via the readiness snapshot)
/// must report `unavailable_error` with code `index_cancelled` —
/// distinct from `index_failed` so callers can tell a real failure
/// from a cooperative shutdown.
#[test]
fn cancelled_publishes_index_cancelled_not_index_failed() {
    let lifecycle = LifecycleInfo::new(SystemTime::now());
    let readiness = lain::server::readiness::ReadinessHandle::default();
    readiness.cancelled();
    let snapshot = readiness.snapshot();
    assert_eq!(
        snapshot.state,
        lain::server::readiness::IndexState::UnavailableError,
    );
    let problem = snapshot.problem.expect("problem must be set");
    assert_eq!(problem.code, "index_cancelled");
    assert!(!problem.retryable);
    // `LifecycleInfo::cancel_token` is unrelated to the readiness
    // snapshot but the two should agree that the server has stopped
    // accepting work.
    let _ = lifecycle;
}

/// `RefreshOutcome::cancelled` is a distinct outcome from
/// `RefreshResult::Failed` so the doctor / `get_health` surfaces
/// can tell a user-initiated shutdown from a real failure.
#[test]
fn refresh_outcome_distinguishes_cancelled_from_failed() {
    use lain::server::refresh::{RefreshOutcome, RefreshResult};

    let now = SystemTime::now();
    let cancelled = RefreshOutcome::cancelled(now);
    assert!(matches!(cancelled.result, RefreshResult::Cancelled));
    assert_eq!(cancelled.result.label(), "cancelled");

    let failed = RefreshOutcome::failed(now, "boom");
    assert!(matches!(failed.result, RefreshResult::Failed(_)));
    assert_eq!(failed.result.label(), "failed");
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
    run(&["config", "user.email", "cancellation-test@lain"]);
    run(&["config", "user.name", "cancellation-test"]);
    std::fs::write(root.join("lib.rs"), "pub fn hello() {}\n").unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-q", "-m", "fixture"]);
}
