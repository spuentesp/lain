//! Battery of positive + negative tests for presence + audit tools.
//!
//! Two layers:
//!   - `PresenceRegistry` (session/heartbeat) — unit-tested directly.
//!   - `OccupancyMap` (claim/release/occupancy) — already heavily
//!     covered by `tests/presence.rs` (20+ tests, all stub-verified
//!     during the wishlist #13 work). Pointer comments below note
//!     which test pins which contract.

use lain::server::presence::{AgentId, AgentKind, AgentMode, PresenceRegistry};

fn fresh_registry() -> PresenceRegistry {
    PresenceRegistry::new()
}

fn register_one(reg: &PresenceRegistry, name: &str) -> lain::server::presence::AgentSession {
    reg.register(
        name.to_string(),
        AgentKind::ClaudeCode,
        AgentMode::Interactive,
        None,
        None,
    )
}

// ─── register / heartbeat ────────────────────────────────────────

#[test]
fn register_assigns_unique_id_and_token() {
    let reg = fresh_registry();
    let s = register_one(&reg, "agent-a");
    assert!(!s.id.as_str().is_empty());
    assert!(!s.session_token.is_empty(),
            "register must assign a session token");
}

#[test]
fn register_assigns_kind_and_mode() {
    let reg = fresh_registry();
    let s = register_one(&reg, "agent-a");
    assert!(matches!(s.kind, AgentKind::ClaudeCode));
    assert!(matches!(s.mode, AgentMode::Interactive));
}

#[test]
fn register_distinguishes_foreground_and_background() {
    let reg = fresh_registry();
    let fg = reg.register("fg".into(), AgentKind::ClaudeCode,
                         AgentMode::Interactive, None, None);
    let bg = reg.register("bg".into(), AgentKind::ClaudeCode,
                         AgentMode::Background, None, None);
    assert!(matches!(fg.mode, AgentMode::Interactive));
    assert!(matches!(bg.mode, AgentMode::Background));
}

#[test]
fn heartbeat_with_correct_token_refreshes() {
    let reg = fresh_registry();
    let s = register_one(&reg, "agent-a");
    assert!(reg.heartbeat(&s.id, &s.session_token).is_ok(),
            "heartbeat with correct token must succeed");
}

#[test]
fn heartbeat_with_wrong_token_errors() {
    let reg = fresh_registry();
    let s = register_one(&reg, "agent-a");
    assert!(reg.heartbeat(&s.id, "definitely_wrong_token_xyz").is_err(),
            "heartbeat with wrong token must error");
}

#[test]
fn heartbeat_for_unknown_agent_errors() {
    let reg = fresh_registry();
    let s = register_one(&reg, "agent-a");
    let unknown = AgentId("00000000-0000-0000-0000-000000000000".into());
    assert!(reg.heartbeat(&unknown, &s.session_token).is_err(),
            "heartbeat for unknown agent must error");
}

#[test]
fn session_token_round_trips_for_heartbeat() {
    let reg = fresh_registry();
    let s = register_one(&reg, "agent-a");
    for _ in 0..3 {
        assert!(reg.heartbeat(&s.id, &s.session_token).is_ok());
    }
}

// ─── list_active ─────────────────────────────────────────────────

#[test]
fn list_active_returns_all_registered_interactive() {
    let reg = fresh_registry();
    register_one(&reg, "agent-a");
    register_one(&reg, "agent-b");
    let active = reg.list_active(false);
    assert_eq!(active.len(), 2, "two interactive agents; got {}", active.len());
}

#[test]
fn list_active_excludes_background_by_default() {
    let reg = fresh_registry();
    reg.register("bg".into(), AgentKind::ClaudeCode, AgentMode::Background, None, None);
    let active = reg.list_active(false);
    assert!(active.is_empty(),
            "list_active(false) must exclude background agents; got {}",
            active.len());
    let all = reg.list_active(true);
    assert_eq!(all.len(), 1, "list_active(true) includes background");
}

#[test]
fn list_active_empty_on_fresh_registry() {
    let reg = fresh_registry();
    assert!(reg.list_active(false).is_empty());
    assert!(reg.list_active(true).is_empty());
}

// ─── session identity ───────────────────────────────────────────

#[test]
fn multiple_agents_get_distinct_tokens() {
    let reg = fresh_registry();
    let s1 = register_one(&reg, "agent-a");
    let s2 = register_one(&reg, "agent-b");
    assert_ne!(s1.session_token, s2.session_token);
    assert_ne!(s1.id.as_str(), s2.id.as_str());
}

#[test]
fn register_twice_with_same_name_still_distinct() {
    let reg = fresh_registry();
    let s1 = register_one(&reg, "agent-a");
    let s2 = register_one(&reg, "agent-a");
    assert_ne!(s1.id.as_str(), s2.id.as_str(),
               "two registrations get distinct ids even with same name");
}

// ─── Pointer comments to OccupancyMap coverage ───────────────────
//
// The claim/release/list_for_path/list_all/occupancy tests live in
// tests/presence.rs, which has 20+ tests covering every wishlist
// pin:
//   - claim_grants_empty_path_when_unoccupied
//   - claim_reports_conflict_on_overlap
//   - claim_different_symbols_on_same_file_no_conflict
//   - claim_file_level_no_symbols_overlaps_with_anything_on_file
//   - release_returns_removed_paths
//   - release_all_for_clears_agent
//   - list_for_path_shows_all_agents
//   - list_all_returns_all_claimed_paths
//   - lain_server_exposes_presence_and_occupancy
//   - presence_tool_dispatchers_round_trip
//   - config_dir_contains_hooks_subdir_helper
//   - symbol_hash_from_bytes_roundtrips
//   - symbol_hash_zero_is_distinct_from_real_hash
//   - persistence_round_trip
//
// detect_overlap lives at the MCP tool layer (LainServer + args JSON)
// and is covered by tests/federation_integration.rs:
//   - detect_overlap_reports_shared_symbols
//   - detect_overlap_rejects_unknown_workspace
//   - detect_overlap_two_shared_functions_is_high
//
// Audit log is covered by tests/audit_integration.rs:
//   - granted_claim_appends_audit_event
//   - rejected_claim_does_not_append_audit_event
//   - multi_file_grant_emits_one_audit_line_per_path
//   - audit_append_failure_does_not_block_claim
#[allow(dead_code)]
const OCCUPANCY_AUDIT_POINTERS: &str = "see tests/presence.rs + tests/audit_integration.rs + tests/federation_integration.rs";
