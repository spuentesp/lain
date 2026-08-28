//! Regression tests for `RepoIndex::Drop`.
//!
//! Background: `RepoIndex::drop` spawns a background thread to shut the LSP
//! pool down cleanly. The original code called `join()` on the thread
//! unconditionally — if the LSP child hung past the `LSP_SHUTDOWN_BUDGET`
//! (10 s), the join blocked indefinitely. For `lain mcp` (stdio transport,
//! current_thread runtime), this parked the only runtime thread until the
//! process was killed.
//!
//! The fix extracts the bounded-wait pattern into a small helper
//! ([`lain::federation::repo_index::run_with_budget`]) and uses it from
//! `Drop`. The helper spawns the thread, waits on a rendezvous channel
//! with a budget, and on timeout drops the `JoinHandle` so the thread
//! runs to completion in the background (the thread holds its own
//! `Arc<LspPool>` clone, which drops the LSP child via `kill_on_drop`
//! when the thread's runtime exits).
//!
//! These tests exercise the helper directly with a controllable closure
//! (true red-green for the budget pattern), then exercise the full
//! `RepoIndex::Drop` plumbing with a real `RepoIndex` (no watcher — see
//! the `notify` 6.1.1 caveat below).

use lain::federation::repo_id::RepoId;
use lain::federation::repo_index::{run_with_budget, RepoIndex};
use lain::federation::repo_source::WorkspaceDirSource;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn init_temp_git_repo(path: &std::path::Path) {
    use std::process::Command;
    Command::new("git")
        .arg("init")
        .arg("-q")
        .arg(path)
        .status()
        .unwrap();
    Command::new("git")
        .args(["-C", path.to_str().unwrap(), "config", "user.email", "t@t"])
        .status()
        .unwrap();
    Command::new("git")
        .args(["-C", path.to_str().unwrap(), "config", "user.name", "t"])
        .status()
        .unwrap();
}

fn build_repo_index(tmp: &tempfile::TempDir) -> RepoIndex {
    let repo_dir = PathBuf::from(tmp.path());
    let src_dir = repo_dir.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("lib.rs"), "pub fn existing() {}\n").unwrap();
    init_temp_git_repo(&repo_dir);

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let source = Box::new(
        WorkspaceDirSource::new(RepoId::new("test").unwrap(), repo_dir).unwrap(),
    );
    RepoIndex::new(source, &data_dir).unwrap()
}

/// The bounded-wait pattern must return promptly when the spawned
/// closure doesn't finish within the budget. Pre-fix (an unbounded
/// `JoinHandle::join()`), the call would have blocked for the full
/// 60 s sleep. After the fix, the call returns at `budget` plus a
/// small grace window for thread teardown.
#[test]
fn run_with_budget_returns_promptly_when_closure_hangs() {
    let start = Instant::now();
    run_with_budget(
        "test-hang",
        || std::thread::sleep(Duration::from_secs(60)),
        Duration::from_millis(100),
    );
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "run_with_budget with a hung closure returned in {elapsed:?}; \
         expected well under the 100ms budget + grace"
    );
}

/// When the closure completes within the budget, the helper joins the
/// thread and we observe the closure's side effect. Pre-fix this also
/// worked; this is a regression guard against a future refactor that
/// drops the JoinHandle unconditionally and races on shutdown.
#[test]
fn run_with_budget_joins_when_closure_completes() {
    let ran = Arc::new(AtomicBool::new(false));
    let ran_for_closure = Arc::clone(&ran);
    let start = Instant::now();
    run_with_budget(
        "test-fast",
        move || {
            ran_for_closure.store(true, Ordering::SeqCst);
        },
        Duration::from_secs(5),
    );
    let elapsed = start.elapsed();
    assert!(
        ran.load(Ordering::SeqCst),
        "closure should have run to completion within the budget"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "fast closure + generous budget returned in {elapsed:?}; expected << 1s"
    );
}

/// `RepoIndex::Drop` is bounded. With no watcher started and no LSP
/// children spawned, `LspPool::shutdown_all` is a no-op, so the
/// background thread returns essentially immediately. This guards
/// against future regressions where someone re-introduces an
/// unbounded wait.
///
/// Note: we deliberately do NOT call `start_watcher` before this
/// test. `notify::RecommendedWatcher::drop` in notify 6.1.1 has a
/// known race that can panic when the inotify event-loop thread has
/// already exited (see tests/federation_integration.rs:184-192 for
/// the upstream context).
#[test]
fn repo_index_drop_is_bounded() {
    let tmp = tempfile::tempdir().unwrap();
    let ri = build_repo_index(&tmp);
    let start = Instant::now();
    drop(ri);
    let elapsed = start.elapsed();
    // LSP_SHUTDOWN_BUDGET is 10 s; we give the runtime check +
    // thread spawn + shutdown + join a generous 12 s ceiling. A real
    // hang would push elapsed well past this.
    assert!(
        elapsed < Duration::from_secs(12),
        "RepoIndex::drop took {elapsed:?}; expected < 12s (10s budget + grace)"
    );
}
