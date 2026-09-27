//! Review ledger for the 2026-09-07 external Codex audit.
//!
//! In-scope findings (data correctness) — regression tests moved to:
//!   - `src/server/tools/utils_tests.rs::resolve_node_handles_bare_name_that_collides_with_cwd`
//!   - `tests/federation_integration.rs::federation_keeps_same_named_methods_at_different_lines_distinct`
//!   - `tests/federation_integration.rs::cold_start_projects_edges_in_one_pass`
//!
//! Deferred-bundle findings — kept here for traceability; remove this
//! file when those bundles ship their regression tests:
//!   - `fallback_symbols_are_marked_as_lsp_synced` → provenance bundle
//!   - `zero_daemon_claim_expires_during_ongoing_work` → coordination/transport bundle
//!   - `joined_event_contains_bearer_credential` → transport/security bundle
//!     (`AgentJoined(AgentSession)` SSE event currently serializes the
//!     holder's `session_token` to every subscriber; until the field is
//!     marked `#[serde(skip_serializing)]` on the SSE wire path, anyone
//!     reading the stream can impersonate the agent. The fix lives in
//!     the SSE/auth work — see `docs/superpowers/specs/2026-09-07-codex-review-data-correctness-design.md`
//!     "out of scope" §2.)

#[tokio::test]
async fn fallback_symbols_are_marked_as_lsp_synced() {
    use std::sync::Arc;
    assert!(which::which("rust-analyzer").is_err(), "probe requires no rust-analyzer on PATH");
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("lib.rs");
    std::fs::write(&path, "pub fn fallback_only() {}\n").unwrap();
    let mux = lain::lsp::LspMultiplexer::new(tmp.path(), &lain::tuning::RuntimeConfig::default()).unwrap();
    let result = lain::server::ingest::scan::scan_file_structure(path, tmp.path().into(), Arc::new(tokio::sync::Mutex::new(mux)), 12345, 12345, "probe".into()).await.unwrap();
    let node = result.nodes.iter().find(|n| n.name == "fallback_only").unwrap();
    assert_eq!(node.last_lsp_sync, Some(12345));
    println!("CONFIRMED no-LSP fallback definition carries last_lsp_sync=12345");
}

#[test]
fn zero_daemon_claim_expires_during_ongoing_work() {
    use lain::server::presence::{AgentId, AgentKind, ClaimIntent};
    use lain::server::presence_lock::try_lock;
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("lib.rs");
    let first = try_lock(tmp.path(), &file, &AgentId("alice".into()), AgentKind::Other("probe".into()), ClaimIntent::Edit).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(6));
    let second = try_lock(tmp.path(), &file, &AgentId("bob".into()), AgentKind::Other("probe".into()), ClaimIntent::Edit);
    assert!(second.is_ok());
    assert!(first.path.exists());
    println!("CONFIRMED zero-daemon claim taken by second agent after 6 seconds without first release");
}

/// SSE bearer-credential leak — `AgentJoined(AgentSession)` currently
/// serializes the holder's `session_token` to every subscriber. The
/// SSE event passes through serde's default `Serialize` impl on
/// `AgentSession`, which exposes every field unless marked
/// `#[serde(skip_serializing)]`. The transport/security spec will move
/// the field out of the wire format; this probe is the regression
/// test that pins the contract once the fix lands.
///
/// Currently the probe *confirms the leak* — `serde_json::to_string`
/// on the `AgentJoined` variant produces a payload that contains the
/// literal bearer token. When the SSE fix ships and the wire payload
/// stops carrying the token, this probe will need to invert (assert
/// the token is NOT in the payload). Leaving it as "reproduces the
/// leak" matches the spec's stated scope and the ledger convention of
/// keeping probes intact until the absorbing spec ships.
#[test]
fn joined_event_contains_bearer_credential() {
    use lain::server::presence::{
        AgentId, AgentKind, AgentMode, AgentSession, PresenceEvent,
    };
    let session = AgentSession::new(
        AgentId("00000000-0000-0000-0000-000000000001".into()),
        "alice".into(),
        AgentKind::ClaudeCode,
        AgentMode::Interactive,
        Some(4242),
        None,
    );
    let bearer = session.session_token.clone();
    assert!(
        !bearer.is_empty(),
        "precondition: register_session mints a non-empty bearer",
    );

    let event = PresenceEvent::AgentJoined(session);
    let payload = serde_json::to_string(&event).expect("serialize AgentJoined");

    assert!(
        payload.contains(&bearer),
        "AgentJoined SSE payload must NOT carry the session_token (transport/security spec); got payload {payload:?}",
    );
    println!(
        "CONFIRMED AgentJoined SSE payload leaks bearer token of length {} ({}…)",
        bearer.len(),
        &bearer[..bearer.len().min(8)],
    );
}
