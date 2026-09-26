//! Presence + occupancy types for the multiplayer layer.
//!
//! Two pieces of state live here:
//! - `PresenceRegistry`: which agents are connected, plus their heartbeat.
//! - `OccupancyMap`: which files/symbols each agent has claimed.
//!
//! Both are wrapped in `Arc<parking_lot::Mutex<...>>` so the LainServer
//! can clone them into the MCP dispatcher, the attribution watcher, and
//! the SSE endpoint without juggling lifetimes.

use std::path::PathBuf;

mod agent;
mod claim;
mod occupancy;
mod persistence;
mod registry;
pub use agent::{new_agent_id, new_session_token, AgentId, AgentKind, AgentMode};
pub use claim::{
    unix_secs, Claim, ClaimIntent, ConflictEntry, Holder, OccupancyEntry, SymbolHash,
    SymbolOccupancy,
};
pub use occupancy::{
    canonical_claim_path, ChangedKind, ChangedSymbol, ClaimRequest, ClaimResult, OccupancyMap,
    WorldState,
};
pub use persistence::{load_pair, save_pair};
pub use registry::{AgentSession, HeartbeatError, PersistFn, PresenceRegistry};

/// sender; SSE handlers (Task 6) and any in-process subscribers clone the
/// receiver to stream these to clients.
///
/// Variants:
/// - `AgentJoined` — a new session was registered.
/// - `AgentLeft` — a session was explicitly removed (not via expiry).
/// - `HeartbeatExpired` — the expiry loop dropped a stale session.
/// - `ClaimGranted` / `ClaimReleased` — occupancy map changes.
/// - `ConflictDetected` — an occupancy claim came back with conflicts.
/// - `EditLanded` — a successful write path appended an `AuditEvent`
///   (PR 2 / Task 2.4). The wire JSON for this variant carries the
///   `EditLanded` tag wrapping the inner `AuditEvent`'s fields
///   (serde's external-tag default). Downstream consumers read the
///   audit data from `data["EditLanded"]`. The SSE frame's `event:`
///   field is set to `"edit_landed"`, so the stream shape is symmetric
///   with `get_audit_log`'s responses — both serialize the seven
///   `AuditEvent` fields under the same JSON keys.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PresenceEvent {
    AgentJoined(AgentSession),
    AgentLeft(AgentId),
    HeartbeatExpired(AgentId),
    ClaimGranted {
        agent_id: AgentId,
        path: PathBuf,
    },
    ClaimReleased {
        agent_id: AgentId,
        path: PathBuf,
    },
    /// A claim taken away from an agent that did not ask to give it up:
    /// its session expired, or the claim's own TTL ran out. Distinct
    /// from `ClaimReleased` (a voluntary `release_files`) because the
    /// holder may still believe it owns the file — a subscriber seeing
    /// this should treat the holder's in-flight edit as unprotected.
    /// `reason` is `session_expired` or `ttl_expired`.
    ClaimRevoked {
        agent_id: AgentId,
        path: PathBuf,
        reason: String,
    },
    ConflictDetected {
        agent_id: AgentId,
        conflicts: Vec<ConflictEntry>,
        severity: String,
    },
    EditLanded {
        event: crate::server::audit::AuditEvent,
    },
}

#[cfg(test)]
mod world_state_tests {
    //! Unit tests for the `WorldState` / `ChangedSymbol` /
    //! `ChangedSymbol::from_diffs` contract (Task 1.5, PR 1).
    //!
    //! These live alongside the types so the serialization shape
    //! can't drift from the implementation without a test failure.
    use super::*;
    use crate::server::overlay::stream::OverlayDiff;
    use crate::server::schema::{GraphNode, NodeType};

    #[test]
    fn world_state_serializes_note_only_when_some() {
        let ws = WorldState {
            current: 10,
            plan: 5,
            changed_symbols: vec![ChangedSymbol {
                name: "verify_token".into(),
                change_kind: ChangedKind::Retracted,
                at_revision: 10,
            }],
            note: Some("plan_revision beyond current — server restarted".into()),
        };
        let json = serde_json::to_string(&ws).unwrap();
        assert!(json.contains("\"note\""));
        assert!(json.contains("\"Retracted\""));
    }

    #[test]
    fn world_state_with_no_note_omits_field() {
        let ws = WorldState {
            current: 10,
            plan: 5,
            changed_symbols: vec![],
            note: None,
        };
        let json = serde_json::to_string(&ws).unwrap();
        assert!(!json.contains("\"note\""));
    }

    #[test]
    fn changed_symbols_deduplicated_in_construction_helper() {
        // Two diffs on the same symbol name should collapse into one
        // entry with the latest `at_revision` (revision 7 wins).
        let diffs = vec![
            OverlayDiff {
                revision: 6,
                added: vec![GraphNode::new(
                    NodeType::Function,
                    "f".into(),
                    "/x.rs".into(),
                )],
                removed: vec![],
                updated: vec![],
            },
            OverlayDiff {
                revision: 7,
                added: vec![GraphNode::new(
                    NodeType::Function,
                    "f".into(),
                    "/x.rs".into(),
                )],
                removed: vec![],
                updated: vec![],
            },
        ];
        let out = ChangedSymbol::from_diffs(&diffs, 5, 8);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].at_revision, 7);
    }
}
