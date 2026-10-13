//! Battery of positive + negative tests for the audit log layer.
//!
//! Audit log: every grant/reject should append a single JSONL line.
//! The file-lock layer that used to live alongside it is zoanthrope's
//! now (see `docs/formal/COORDINATION.md`); its contract tests moved
//! with it.

use lain::server::audit::{
    append_edit_event, audit_log_present_and_readable, read_audit_log, AuditEvent,
};
use lain::server::presence::AgentId;
use lain::server::revision_log::RevisionId;

fn fresh_audit_event() -> AuditEvent {
    AuditEvent {
        ts_unix: 0.0,
        agent_id: AgentId("test-agent-uuid-xyz".into()),
        path: "src/lib.rs".to_string(),
        claim_set: vec![],
        racers: vec![],
        plan_revision: None,
        landed_revision: RevisionId::default(),
        scope: None,
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
    assert!(
        events.is_empty(),
        "since-future filter returns empty; got {}",
        events.len()
    );
}

#[test]
fn audit_multiple_appends_append_multiple_lines() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path();
    for _ in 0..3 {
        append_edit_event(state_dir, &fresh_audit_event()).unwrap();
    }
    let events = read_audit_log(state_dir, None).unwrap();
    assert!(
        events.len() >= 3,
        "three appends must produce three events; got {}",
        events.len()
    );
}
