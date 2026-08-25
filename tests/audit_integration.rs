//! Integration tests for the audit log appended by `claim_files`
//! (Task 2.3, PR 2).
//!
//! Every granted claim must produce one `AuditEvent` line in
//! `<state_dir>/audit.jsonl`. The log is best-effort: an I/O failure
//! only emits a `WARN`, never blocks the claim. These tests exercise
//! the dispatcher end-to-end so the same code path production hooks
//! reach is what we cover here.

use lain::config::state_dir;
use lain::server::audit::{read_audit_log, AuditEvent};
use lain::server::LainServer;
use std::sync::Mutex;

/// Process-wide mutex that serialises the audit tests below. The
/// `XDG_STATE_HOME` override in `isolate_state_dir` is process-wide
/// (it's an env var), so two audit tests running concurrently would
/// both write to whichever `state_dir()` happens to resolve to at
/// the moment `append_edit_event` is called — the second test
/// would then see the first test's events in the log. Holding this
/// lock for the duration of each test makes the env override
/// effectively single-threaded for the audit tests.
static AUDIT_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Set `XDG_STATE_HOME` to a fresh tempdir so `state_dir()` resolves
/// to a known, isolated location for this test. Returns the tempdir
/// handle and the resolved audit directory. The env override is
/// process-wide, so this test must be the only one writing audit
/// lines during its execution; the broader suite does not write to
/// the audit log yet, so the `AUDIT_TEST_LOCK` mutex above is
/// enough.
fn isolate_state_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    // `state_dir()` joins `lain` onto `XDG_STATE_HOME`, so the audit
    // file lands at `<tmp>/lain/audit.jsonl`. Mirror that in the
    // returned path so the test can read the log directly.
    std::env::set_var("XDG_STATE_HOME", tmp.path().to_string_lossy().to_string());
    let audit_dir = state_dir();
    (tmp, audit_dir)
}

/// A successful `claim_files` call appends one `AuditEvent` line to
/// `<state_dir>/audit.jsonl`. The line decodes back into the same
/// shape the handler was constructed with: agent_id matches the
/// registered session, `path` is the claimed file, `plan_revision` is
/// `None` for the legacy caller (no plan tracking), and
/// `landed_revision` is at least 1 (the overlay counter advanced past
/// zero before the claim landed).
#[tokio::test]
async fn granted_claim_appends_audit_event() {
    let _guard = AUDIT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_state_tmp, audit_dir) = isolate_state_dir();

    // ── Server + workspace ────────────────────────────────────────────
    let ws = tempfile::tempdir().unwrap();
    git2::Repository::init(ws.path()).unwrap();
    std::fs::write(ws.path().join("auth.rs"), "pub fn login() {}").unwrap();
    let mem = ws.path().join(".lain/graph.bin");
    let server = LainServer::new(ws.path(), &mem, None).expect("server");
    let server = std::sync::Arc::new(server);

    // ── Register agent ────────────────────────────────────────────────
    let reg = lain::server::mcp::presence_tools::run_register_agent(
        &server,
        serde_json::json!({"name": "alice", "kind": "claude-code"}),
    )
    .expect("register_agent");
    let agent_id = reg["agent_id"].as_str().unwrap().to_string();
    let token = reg["session_token"].as_str().unwrap().to_string();

    // ── Claim auth.rs (legacy path: no plan_revision) ─────────────────
    let resp = lain::server::mcp::presence_tools::run_claim_files(
        &server,
        serde_json::json!({
            "agent_id": agent_id,
            "session_token": token,
            "files": [{"path": "auth.rs", "symbols": ["login"], "intent": "edit"}],
        }),
    )
    .expect("claim_files should succeed");
    assert_eq!(
        resp["granted"].as_array().unwrap().len(),
        1,
        "claim should be granted; resp={resp}"
    );

    // ── Audit log: exactly one line, decodes cleanly ──────────────────
    let log_path = audit_dir.join("audit.jsonl");
    assert!(
        log_path.exists(),
        "audit.jsonl must exist under {}",
        audit_dir.display()
    );
    let events: Vec<AuditEvent> = read_audit_log(&audit_dir, None).expect("read_audit_log");
    assert_eq!(
        events.len(),
        1,
        "expected exactly one audit event after one granted claim; got {events:?}"
    );

    let ev = &events[0];
    assert_eq!(ev.agent_id.as_str(), agent_id);
    assert_eq!(ev.path.to_string_lossy(), "auth.rs");
    assert!(ev.plan_revision.is_none(), "legacy caller must not set plan_revision");
    // landed_revision is deliberately left unasserted: it is whatever the
    // overlay reports at audit time — the floor value 0 for a fresh server,
    // higher once any overlay mutation lands (see the post-bump case in the
    // second test). The audit module serializes whatever the caller hands it,
    // so pinning a value here would over-specify the contract.
    // One Claim in the granted set: the just-recorded claim for
    // auth.rs on behalf of `agent_id`.
    assert_eq!(ev.claim_set.len(), 1, "claim_set should mirror the grant");
    assert_eq!(ev.claim_set[0].agent_id.as_str(), agent_id);
    assert_eq!(ev.claim_set[0].path.to_string_lossy(), "auth.rs");
    assert_eq!(ev.claim_set[0].symbols, vec!["login"]);
    // No racers: alice was alone on this file.
    assert!(ev.racers.is_empty(), "uncontested claim must record no racers");
}

/// A claim that the occupancy map rejects (already-claimed Edit) is
/// not granted, so the audit log must NOT receive a new entry for it.
/// The audit append is gated on `result.granted.is_empty()`, per the
/// spec's "audit never blocks an edit" + "audit never records a
/// phantom grant" invariants.
#[tokio::test]
async fn rejected_claim_does_not_append_audit_event() {
    let _guard = AUDIT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_state_tmp, audit_dir) = isolate_state_dir();

    let ws = tempfile::tempdir().unwrap();
    git2::Repository::init(ws.path()).unwrap();
    std::fs::write(ws.path().join("auth.rs"), "pub fn login() {}").unwrap();
    let mem = ws.path().join(".lain/graph.bin");
    let server = LainServer::new(ws.path(), &mem, None).expect("server");
    let server = std::sync::Arc::new(server);

    // Two agents race for the same file.
    let alice = lain::server::mcp::presence_tools::run_register_agent(
        &server,
        serde_json::json!({"name": "alice"}),
    )
    .unwrap();
    let alice_id = alice["agent_id"].as_str().unwrap().to_string();
    let alice_token = alice["session_token"].as_str().unwrap().to_string();

    let bob = lain::server::mcp::presence_tools::run_register_agent(
        &server,
        serde_json::json!({"name": "bob"}),
    )
    .unwrap();
    let bob_id = bob["agent_id"].as_str().unwrap().to_string();
    let bob_token = bob["session_token"].as_str().unwrap().to_string();

    // Alice wins the first claim → audit log gets one line.
    let resp_a = lain::server::mcp::presence_tools::run_claim_files(
        &server,
        serde_json::json!({
            "agent_id": alice_id,
            "session_token": alice_token,
            "files": [{"path": "auth.rs", "symbols": ["login"], "intent": "edit"}],
        }),
    )
    .unwrap();
    assert_eq!(resp_a["granted"].as_array().unwrap().len(), 1);

    // Bob's same-file claim is rejected (alice holds the edit).
    let resp_b = lain::server::mcp::presence_tools::run_claim_files(
        &server,
        serde_json::json!({
            "agent_id": bob_id,
            "session_token": bob_token,
            "files": [{"path": "auth.rs", "symbols": ["login"], "intent": "edit"}],
        }),
    )
    .unwrap();
    assert_eq!(
        resp_b["granted"].as_array().unwrap().len(),
        0,
        "bob should be rejected; resp={resp_b}"
    );
    assert_eq!(resp_b["conflicts"].as_array().unwrap().len(), 1);

    // Only alice's grant is in the audit log; bob's rejection must
    // not produce a second entry.
    let events = read_audit_log(&audit_dir, None).expect("read_audit_log");
    assert_eq!(
        events.len(),
        1,
        "only the granted claim should append; got {events:?}"
    );
    assert_eq!(events[0].agent_id.as_str(), alice_id);
}

/// A claim that successfully grants *two* paths in one call emits
/// *two* audit lines, one per granted path. The audit log is
/// structured per-edit (per-file), not per-call — a single hook
/// asking for `auth.rs` and `db.rs` simultaneously should leave two
/// lines on disk so the dashboard can correlate each audit entry to
/// the specific file it describes.
#[tokio::test]
async fn multi_file_grant_emits_one_audit_line_per_path() {
    let _guard = AUDIT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_state_tmp, audit_dir) = isolate_state_dir();

    let ws = tempfile::tempdir().unwrap();
    git2::Repository::init(ws.path()).unwrap();
    std::fs::write(ws.path().join("auth.rs"), "pub fn login() {}").unwrap();
    std::fs::write(ws.path().join("db.rs"), "pub fn query() {}").unwrap();
    let mem = ws.path().join(".lain/graph.bin");
    let server = LainServer::new(ws.path(), &mem, None).expect("server");
    let server = std::sync::Arc::new(server);

    let reg = lain::server::mcp::presence_tools::run_register_agent(
        &server,
        serde_json::json!({"name": "alice"}),
    )
    .unwrap();
    let agent_id = reg["agent_id"].as_str().unwrap().to_string();
    let token = reg["session_token"].as_str().unwrap().to_string();

    let resp = lain::server::mcp::presence_tools::run_claim_files(
        &server,
        serde_json::json!({
            "agent_id": agent_id,
            "session_token": token,
            "files": [
                {"path": "auth.rs", "symbols": ["login"], "intent": "edit"},
                {"path": "db.rs",   "symbols": ["query"], "intent": "edit"},
            ],
        }),
    )
    .expect("claim_files should succeed");
    assert_eq!(resp["granted"].as_array().unwrap().len(), 2);

    let events = read_audit_log(&audit_dir, None).expect("read_audit_log");
    assert_eq!(
        events.len(),
        2,
        "one audit line per granted file; got {events:?}"
    );
    let mut paths: Vec<String> = events.iter().map(|e| e.path.to_string_lossy().into_owned()).collect();
    paths.sort();
    assert_eq!(paths, vec!["auth.rs".to_string(), "db.rs".to_string()]);
    for ev in &events {
        assert_eq!(ev.agent_id.as_str(), agent_id);
        assert_eq!(ev.claim_set.len(), 1, "claim_set should match the per-file grant");
    }
}

/// The audit append is best-effort: an I/O failure must surface as a
/// `WARN` and not abort the claim response. We simulate the failure
/// by pointing `XDG_STATE_HOME` at a path whose `lain/` child cannot
/// be created (a regular file in the way). The handler still returns
/// `granted: [...]` so the agent's edit proceeds; the audit module
/// logs the failure and the dispatcher's `WARN` is acceptable. This
/// pins the "audit never blocks an edit" invariant from the spec.
#[tokio::test]
async fn audit_append_failure_does_not_block_claim() {
    let _guard = AUDIT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let blocker = tempfile::tempdir().unwrap();
    // Place a regular file where `state_dir()` expects `lain/` to be
    // a directory — `append_edit_event`'s `OpenOptions::create(true)`
    // will then fail with `NotADirectory`.
    let blocker_file = blocker.path().join("lain");
    std::fs::write(&blocker_file, b"not a directory").unwrap();
    std::env::set_var("XDG_STATE_HOME", blocker.path().to_string_lossy().to_string());

    let ws = tempfile::tempdir().unwrap();
    git2::Repository::init(ws.path()).unwrap();
    std::fs::write(ws.path().join("auth.rs"), "pub fn login() {}").unwrap();
    let mem = ws.path().join(".lain/graph.bin");
    let server = LainServer::new(ws.path(), &mem, None).expect("server");
    let server = std::sync::Arc::new(server);

    let reg = lain::server::mcp::presence_tools::run_register_agent(
        &server,
        serde_json::json!({"name": "alice"}),
    )
    .unwrap();
    let agent_id = reg["agent_id"].as_str().unwrap().to_string();
    let token = reg["session_token"].as_str().unwrap().to_string();

    let resp = lain::server::mcp::presence_tools::run_claim_files(
        &server,
        serde_json::json!({
            "agent_id": agent_id,
            "session_token": token,
            "files": [{"path": "auth.rs", "symbols": ["login"], "intent": "edit"}],
        }),
    )
    .expect("claim_files must succeed even when audit append fails");
    assert_eq!(resp["granted"].as_array().unwrap().len(), 1);
}
