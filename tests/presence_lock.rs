//! Integration tests for the filesystem-as-lock layer
//! (`lain::server::presence_lock`).
//!
//! Exercises [`try_lock`] / [`release_lock`] / [`refresh_lock`] against
//! a real tempdir; no lain-server involvement. The internal OccupancyMap
//! integration is covered by the existing `tests/presence.rs` suite —
//! adding `claim_with_session` there would just duplicate what the in-
//! memory layer already tests.

use lain::server::presence::AgentId;
use lain::server::presence_lock::{release_lock, try_lock};
use lain::server::presence::{AgentKind, ClaimIntent};

fn make_agent(id: &str) -> AgentId {
    AgentId(format!("{id}-{}", std::process::id()).into())
}

/// Acquire a fresh lock and confirm both that the helper returned
/// successfully *and* that the sentinel file landed on disk under
/// `<workspace>/.lain/locks/`. Releasing the lock must remove the file
/// (idempotently).
#[test]
fn try_lock_acquires_release_releases() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    let path = ws.join("foo.rs");
    let agent = make_agent("alice");
    let lock = try_lock(ws, &path, &agent, AgentKind::ClaudeCode, ClaimIntent::Edit)
        .expect("lock");
    assert!(lock.path.exists());
    release_lock(&lock).unwrap();
    assert!(!lock.path.exists());
}

/// A second `try_lock` for the same path while the first is fresh
/// returns `LockConflict`, and the reported holder matches alice's
/// agent id. The in-memory layer is unaffected (this test uses no
/// `OccupancyMap`).
#[test]
fn try_lock_returns_conflict_on_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    let path = ws.join("foo.rs");
    let alice = make_agent("alice");
    let bob = make_agent("bob");
    let first = try_lock(ws, &path, &alice, AgentKind::ClaudeCode, ClaimIntent::Edit)
        .expect("first");
    let second = try_lock(ws, &path, &bob, AgentKind::ClaudeCode, ClaimIntent::Edit);
    assert!(second.is_err());
    let conflict = second.unwrap_err();
    assert_eq!(conflict.agent_id(), alice);
    release_lock(&first).unwrap();
}

/// When the existing lock is older than the TTL window (here simulated
/// by `set_file_mtime` to UNIX_EPOCH + 1s), a competing agent *can*
/// take it. The mtime check + stale-removal path is the only thing
/// that makes this layer safe against dead writers; this test is the
/// contract that says "stale means stealable."
#[test]
fn stale_lock_can_be_taken_after_mtime_window() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    let path = ws.join("foo.rs");
    let alice = make_agent("alice");
    let bob = make_agent("bob");
    let first = try_lock(ws, &path, &alice, AgentKind::ClaudeCode, ClaimIntent::Edit)
        .expect("first");
    // Backdate the lock file's mtime to simulate a dead writer. Uses
    // `File::set_modified` (stable) rather than the nightly-only
    // `std::fs::set_file_mtime`.
    let past = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
    {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&first.path)
            .unwrap();
        f.set_modified(past).unwrap();
    }
    let second = try_lock(ws, &path, &bob, AgentKind::Kimi, ClaimIntent::Read)
        .expect("stale lock taken");
    release_lock(&second).unwrap();
}

/// `refresh_lock` must bump the sentinel file's mtime within the TTL
/// window so a competing `try_lock` doesn't think the holder is dead.
/// Read-back via `metadata().modified()` proves the kernel honored
/// `set_file_mtime` (cheap guard against filesystems that don't
/// preserve mtime).
#[test]
fn refresh_lock_keeps_lock_alive() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    let path = ws.join("foo.rs");
    let agent = make_agent("alice");
    let lock = try_lock(ws, &path, &agent, AgentKind::ClaudeCode, ClaimIntent::Edit)
        .expect("lock");
    let mtime_before = std::fs::metadata(&lock.path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    lock.refresh_lock().unwrap();
    let mtime_after = std::fs::metadata(&lock.path).unwrap().modified().unwrap();
    assert!(mtime_after > mtime_before);
    release_lock(&lock).unwrap();
}
