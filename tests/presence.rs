//! Session-registry tests — the part of the old `tests/presence.rs`
//! that outlived the coordination refactor.
//!
//! The in-memory claims registry (`OccupancyMap`) and its arbitration
//! tests are gone with the core: claims live in zoanthrope now and the
//! proxy contract is pinned in `tests/coordination_proxy.rs`. What
//! remains genuinely lain's is the session auth table
//! (`PresenceRegistry`): register → `session_token` → heartbeat →
//! expiry.

use lain::server::presence::{
    new_agent_id, new_session_token, AgentId, AgentKind, AgentMode, ClaimIntent, PresenceRegistry,
};
use std::time::Duration;

#[test]
fn register_mints_distinct_sessions() {
    let reg = PresenceRegistry::new();
    let a = reg.register(
        "alice".into(),
        AgentKind::ClaudeCode,
        AgentMode::Interactive,
        None,
        None,
    );
    let b = reg.register(
        "bob".into(),
        AgentKind::Kimi,
        AgentMode::Interactive,
        None,
        None,
    );
    assert_ne!(a.id, b.id);
    assert_ne!(a.session_token, b.session_token);
    assert_eq!(a.name, "alice");
    assert_eq!(a.kind.as_str(), "claude-code");
}

#[test]
fn session_token_resolves_to_its_owner() {
    let reg = PresenceRegistry::new();
    let a = reg.register(
        "alice".into(),
        AgentKind::ClaudeCode,
        AgentMode::Interactive,
        None,
        None,
    );
    let resolved = reg.by_token(&a.session_token).expect("token resolves");
    assert_eq!(resolved.id, a.id);
    assert!(reg.by_token("nope").is_none());
}

#[test]
fn heartbeat_rejects_wrong_token() {
    let reg = PresenceRegistry::new();
    let a = reg.register(
        "alice".into(),
        AgentKind::ClaudeCode,
        AgentMode::Interactive,
        None,
        None,
    );
    assert!(reg.heartbeat(&a.id, &a.session_token).is_ok());
    assert!(reg.heartbeat(&a.id, "wrong").is_err());
    assert!(reg
        .heartbeat(&AgentId("ghost".into()), &a.session_token)
        .is_err());
}

#[test]
fn stale_sessions_expire_and_fresh_ones_survive() {
    let reg = PresenceRegistry::with_expiry(Duration::from_millis(20));
    let a = reg.register(
        "alice".into(),
        AgentKind::ClaudeCode,
        AgentMode::Interactive,
        None,
        None,
    );
    let b = reg.register(
        "bob".into(),
        AgentKind::Kimi,
        AgentMode::Interactive,
        None,
        None,
    );
    // Keep bob alive; let alice go stale.
    std::thread::sleep(Duration::from_millis(50));
    assert!(reg.heartbeat(&b.id, &b.session_token).is_ok());
    let expired = reg.expire_stale();
    assert!(
        expired.iter().any(|s| s.id == a.id),
        "stale session must expire; expired={expired:?}"
    );
    assert!(
        !expired.iter().any(|s| s.id == b.id),
        "heartbeated session must survive"
    );
    assert!(reg.get(&a.id).is_none());
    assert!(reg.get(&b.id).is_some());
}

#[test]
fn remove_drops_the_session_and_its_token() {
    let reg = PresenceRegistry::new();
    let a = reg.register(
        "alice".into(),
        AgentKind::ClaudeCode,
        AgentMode::Interactive,
        None,
        None,
    );
    assert!(reg.remove(&a.id).is_some());
    assert!(reg.get(&a.id).is_none());
    assert!(reg.by_token(&a.session_token).is_none());
}

#[test]
fn agent_id_and_session_token_are_opaque_and_unique() {
    let a = new_agent_id();
    let b = new_agent_id();
    assert_ne!(a.as_str(), b.as_str());
    assert_ne!(new_session_token(), new_session_token());
}

#[test]
fn claim_intent_wire_spelling_matches_zoanthrope() {
    assert_eq!(ClaimIntent::Read.as_str(), "read");
    assert_eq!(ClaimIntent::Edit.as_str(), "edit");
}
