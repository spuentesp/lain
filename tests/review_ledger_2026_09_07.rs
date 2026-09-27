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
