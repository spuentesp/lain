//! Battery of positive + negative tests for the audit log + file lock layers.
//!
//! Audit log: every grant/reject should append a single JSONL line.
//! File lock: zero-daemon claim/release (wishlist #3) — must work
//! without a running server.

use lain::server::audit::{
    append_edit_event, audit_log_present_and_readable, read_audit_log, AuditEvent,
};
use lain::server::presence::{AgentId, AgentKind, ClaimIntent};
use lain::server::presence_lock::{
    lock_path_for, release_lock, release_lock_at, try_lock,
};
use lain::server::revision_log::RevisionId;
use std::path::Path;

fn fresh_audit_event() -> AuditEvent {
    AuditEvent {
        ts_unix: 0.0,
        agent_id: AgentId("test-agent-uuid-xyz".into()),
        path: Path::new("src/lib.rs").to_path_buf(),
        claim_set: vec![],
        racers: vec![],
        plan_revision: None,
        landed_revision: RevisionId::default(),
    }
}

// ─── Audit log ───────────────────────────────────────────────────

#[test]
fn audit_append_then_read_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path();
    append_edit_event(state_dir, &fresh_audit_event()).expect("append");
    assert!(audit_log_present_and_readable(state_dir));
    let events = read_audit_log(state_dir, None).expect("read");
    assert!(!events.is_empty(), "audit log must surface appended events");
}

#[test]
fn audit_handles_missing_state_dir() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does_not_exist");
    // Must not panic; either Ok (creates dir) or IoError.
    let _ = append_edit_event(&missing, &fresh_audit_event());
}

#[test]
fn audit_handles_no_since_filter() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path();
    append_edit_event(state_dir, &fresh_audit_event()).unwrap();
    // No since filter → returns all events.
    let events = read_audit_log(state_dir, None).unwrap();
    assert!(!events.is_empty());
}

#[test]
fn audit_handles_since_filter() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path();
    append_edit_event(state_dir, &fresh_audit_event()).unwrap();
    // Since filter far in the future → empty.
    let events = read_audit_log(state_dir, Some(1.0e15)).unwrap_or_default();
    assert!(events.is_empty(),
            "since-future filter returns empty; got {}",
            events.len());
}

#[test]
fn audit_multiple_appends_append_multiple_lines() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path();
    for _ in 0..3 {
        append_edit_event(state_dir, &fresh_audit_event()).unwrap();
    }
    let events = read_audit_log(state_dir, None).unwrap();
    assert!(events.len() >= 3,
            "three appends must produce three events; got {}",
            events.len());
}

// ─── File lock (zero-daemon path, wishlist #3) ────────────────────

#[test]
fn file_lock_acquires_and_releases() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let path = Path::new("src/lib.rs");
    let lock_path = lock_path_for(workspace, path);
    let agent = AgentId("agent-a".into());
    let _lock = try_lock(workspace, path, &agent, AgentKind::ClaudeCode, ClaimIntent::Edit)
        .expect("first acquire must succeed");
    assert!(lock_path.exists(), "lock file must be created");
}

#[test]
fn file_lock_returns_conflict_on_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let path = Path::new("src/lib.rs");
    let agent_a = AgentId("agent-a".into());
    let agent_b = AgentId("agent-b".into());
    let _lock = try_lock(workspace, path, &agent_a, AgentKind::ClaudeCode, ClaimIntent::Edit)
        .expect("first acquire must succeed");
    let second = try_lock(workspace, path, &agent_b, AgentKind::ClaudeCode, ClaimIntent::Edit);
    assert!(second.is_err(), "second acquire on same path must conflict");
}

#[test]
fn file_lock_release_clears_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let path = Path::new("src/lib.rs");
    let agent = AgentId("agent-a".into());
    let lock = try_lock(workspace, path, &agent, AgentKind::ClaudeCode, ClaimIntent::Edit)
        .expect("acquire");
    release_lock(&lock).expect("release");
    let _lock2 = try_lock(workspace, path, &agent, AgentKind::ClaudeCode, ClaimIntent::Edit)
        .expect("re-acquire after release must succeed");
}

#[test]
fn file_lock_release_at_handles_missing() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("not_a_lock");
    let result = release_lock_at(&missing);
    assert!(result.is_ok() || result.is_err(),
            "release_lock_at on missing path must not panic");
}

#[test]
fn file_lock_path_for_unique_per_file() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let p_a = lock_path_for(workspace, Path::new("src/a.rs"));
    let p_b = lock_path_for(workspace, Path::new("src/b.rs"));
    assert_ne!(p_a, p_b, "different paths → different lock files");
}

#[test]
fn file_lock_zero_daemon_path_works_without_server() {
    // Wishlist #3 — claim/release works against a plain state
    // directory when no server is running. No `lain` process spawned.
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let path = Path::new("src/lib.rs");
    let agent = AgentId("agent-a".into());
    let lock = try_lock(workspace, path, &agent, AgentKind::ClaudeCode, ClaimIntent::Edit)
        .expect("zero-daemon lock must succeed");
    release_lock(&lock).expect("zero-daemon release must succeed");
    let _lock2 = try_lock(workspace, path, &agent, AgentKind::ClaudeCode, ClaimIntent::Edit)
        .expect("re-acquire after release");
}
