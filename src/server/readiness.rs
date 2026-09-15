//! Shared wire types and readiness policy for CLI and MCP diagnostics.
//!
//! This module performs no probes or I/O. Callers must supply observed capability
//! and transport state; a capability must never be marked ready by default.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Increment only for incompatible changes; consumers may ignore added fields.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexState {
    WarmingUp,
    Ready,
    UnavailableError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexPhase {
    Discovering,
    Scanning,
    Resolving,
    Enriching,
    Persisting,
    Overlay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    pub code: String,
    pub message: String,
    pub remediation: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexLifecycleSnapshot {
    pub sequence: u64,
    pub attempt_id: u64,
    pub state: IndexState,
    pub phase: IndexPhase,
    pub started_at_unix_ms: u64,
    pub completed_at_unix_ms: Option<u64>,
    pub target_commit: Option<String>,
    pub indexed_commit: Option<String>,
    pub files_total: Option<u64>,
    pub files_completed: u64,
    pub files_failed: u64,
    pub retry_after_ms: Option<u64>,
    pub problem: Option<Problem>,
    pub warnings: Vec<Problem>,
}

impl IndexLifecycleSnapshot {
    pub fn warming_up() -> Self {
        Self {
            sequence: 0,
            attempt_id: 1,
            state: IndexState::WarmingUp,
            phase: IndexPhase::Discovering,
            started_at_unix_ms: unix_ms(),
            completed_at_unix_ms: None,
            target_commit: None,
            indexed_commit: None,
            files_total: None,
            files_completed: 0,
            files_failed: 0,
            retry_after_ms: Some(3000),
            problem: None,
            warnings: Vec::new(),
        }
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[derive(Debug, Clone)]
pub struct ReadinessHandle(Arc<parking_lot::Mutex<IndexLifecycleSnapshot>>);

impl Default for ReadinessHandle {
    fn default() -> Self {
        Self(Arc::new(parking_lot::Mutex::new(
            IndexLifecycleSnapshot::warming_up(),
        )))
    }
}

impl ReadinessHandle {
    pub fn snapshot(&self) -> IndexLifecycleSnapshot {
        self.0.lock().clone()
    }

    pub fn update(&self, update: impl FnOnce(&mut IndexLifecycleSnapshot)) {
        let mut snapshot = self.0.lock();
        update(&mut snapshot);
        snapshot.sequence = snapshot.sequence.saturating_add(1);
        snapshot
            .warnings
            .sort_by(|a, b| (&a.code, &a.message).cmp(&(&b.code, &b.message)));
    }

    pub fn ready(&self, indexed_commit: Option<String>) {
        self.update(|snapshot| {
            snapshot.state = IndexState::Ready;
            snapshot.phase = IndexPhase::Overlay;
            snapshot.indexed_commit = indexed_commit;
            snapshot.completed_at_unix_ms = Some(unix_ms());
            snapshot.retry_after_ms = None;
            snapshot.problem = None;
        });
    }

    /// Transition a `Ready` snapshot back to `WarmingUp` for a real
    /// re-index attempt (a commit-sync or watcher-triggered rebuild, not
    /// the no-op "already up to date" fast path, which must never touch
    /// this handle at all). Without this, a re-index after the first
    /// successful pass left `state` at `Ready` the whole time it ran,
    /// so the central gate kept dispatching `graph_required`/
    /// `semantic_required` tools against a graph being actively
    /// mutated instead of turning them back with `warming_up` until the
    /// new pass publishes `ready` again.
    pub fn resume_warming_up(&self) {
        self.update(|snapshot| {
            snapshot.state = IndexState::WarmingUp;
            snapshot.attempt_id = snapshot.attempt_id.saturating_add(1);
            snapshot.completed_at_unix_ms = None;
            snapshot.retry_after_ms = Some(WARMING_UP_RETRY_AFTER_MS);
            snapshot.problem = None;
        });
    }

    pub fn failed(&self, message: String) {
        self.update(|snapshot| {
            snapshot.state = IndexState::UnavailableError;
            snapshot.completed_at_unix_ms = Some(unix_ms());
            snapshot.retry_after_ms = None;
            snapshot.problem = Some(Problem {
                code: "index_failed".into(), message,
                remediation: "Run `lain doctor --json` and retry indexing after correcting the reported problem.".into(),
                retryable: true,
            });
        });
    }
}

/// Schema-version-1 contract: the server does not adjust this per attempt.
/// Changing it requires an explicit schema decision, not an environment knob.
const WARMING_UP_RETRY_AFTER_MS: u64 = 3000;

/// Progress fields mirrored into the gated envelope. A subset of
/// [`IndexLifecycleSnapshot`] — no `attempt_id`/`sequence`, which live at
/// the envelope's top level instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GatedProgress {
    pub phase: IndexPhase,
    pub files_completed: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_total: Option<u64>,
    pub files_failed: u64,
}

/// Always points the caller back at the one canonical polling target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GatedNextAction {
    pub tool: &'static str,
    pub arguments: serde_json::Value,
}

impl GatedNextAction {
    fn poll_capabilities() -> Self {
        Self {
            tool: "get_capabilities",
            arguments: serde_json::json!({}),
        }
    }
}

/// The stable envelope a gated `graph_required` / `semantic_required` tool
/// call returns instead of entering its handler. Field order here is the
/// wire order — this type derives `Serialize` directly rather than going
/// through a builder that could reorder fields between releases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GatedResponse {
    pub schema_version: u32,
    pub attempt_id: u64,
    pub sequence: u64,
    pub state: &'static str,
    pub capability: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<GatedProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<GatedNextAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<Problem>,
    /// Sorted repo ids blocking a federation-wide tool call. Always empty
    /// for a single-workspace response (there is nothing to disambiguate).
    /// Populated by `federation::readiness::gate_federated_tool_call`.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub blocking_repos: Vec<String>,
}

impl GatedResponse {
    /// Whether the MCP `CallToolResult` this projects into must carry
    /// `isError: true`. Only a terminal indexing failure is an error; a
    /// warming or optional-capability response is an expected, retryable
    /// state and must not look like a failed tool call to the model.
    pub fn is_error(&self) -> bool {
        self.state == "unavailable_error"
    }

    fn warming_up(lifecycle: &IndexLifecycleSnapshot, capability: &'static str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            attempt_id: lifecycle.attempt_id,
            sequence: lifecycle.sequence,
            state: "warming_up",
            capability,
            message: "LAIN is indexing this repository.".to_string(),
            progress: Some(GatedProgress {
                phase: lifecycle.phase,
                files_completed: lifecycle.files_completed,
                files_total: lifecycle.files_total,
                files_failed: lifecycle.files_failed,
            }),
            retry_after_ms: Some(WARMING_UP_RETRY_AFTER_MS),
            next_action: Some(GatedNextAction::poll_capabilities()),
            problem: None,
            blocking_repos: Vec::new(),
        }
    }

    fn unavailable_error(lifecycle: &IndexLifecycleSnapshot, capability: &'static str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            attempt_id: lifecycle.attempt_id,
            sequence: lifecycle.sequence,
            state: "unavailable_error",
            capability,
            message: "LAIN could not finish indexing this repository.".to_string(),
            progress: None,
            retry_after_ms: None,
            next_action: Some(GatedNextAction::poll_capabilities()),
            problem: lifecycle.problem.clone(),
            blocking_repos: Vec::new(),
        }
    }

    fn unavailable_optional(lifecycle: &IndexLifecycleSnapshot) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            attempt_id: lifecycle.attempt_id,
            sequence: lifecycle.sequence,
            state: "unavailable_optional",
            capability: "semantic_search",
            message: "Semantic search is optional and no embedding model is installed.".into(),
            progress: None,
            retry_after_ms: None,
            next_action: Some(GatedNextAction::poll_capabilities()),
            problem: None,
            blocking_repos: Vec::new(),
        }
    }
}

/// The one central readiness gate every `tools/call` dispatcher must consult
/// before entering a tool's handler. `None` means "dispatch normally" —
/// either the tool is `graph_independent`, or its required capability is
/// already `ready`, or `name` carries no classification at all (an unknown
/// tool falls through to the ordinary unknown-tool error path rather than
/// being silently gated).
///
/// Handlers must not implement their own startup checks; this is the only
/// place a tool call is denied for readiness reasons.
pub fn gate_tool_call(
    name: &str,
    lifecycle: &IndexLifecycleSnapshot,
    semantic_model_configured: bool,
) -> Option<GatedResponse> {
    use crate::server::tools::definitions::ReadinessRequirement;

    match crate::server::tools::definitions::readiness_requirement(name)? {
        ReadinessRequirement::GraphIndependent => None,
        ReadinessRequirement::GraphRequired => match lifecycle.state {
            IndexState::Ready => None,
            IndexState::WarmingUp => Some(GatedResponse::warming_up(lifecycle, "symbols")),
            IndexState::UnavailableError => {
                Some(GatedResponse::unavailable_error(lifecycle, "symbols"))
            }
        },
        ReadinessRequirement::SemanticRequired => {
            if !semantic_model_configured {
                return Some(GatedResponse::unavailable_optional(lifecycle));
            }
            match lifecycle.state {
                IndexState::Ready => None,
                IndexState::WarmingUp => {
                    Some(GatedResponse::warming_up(lifecycle, "semantic_search"))
                }
                IndexState::UnavailableError => Some(GatedResponse::unavailable_error(
                    lifecycle,
                    "semantic_search",
                )),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    Ready,
    WarmingUp,
    /// Only valid for an intact snapshot isolated from an in-progress rebuild.
    StaleUsable,
    UnavailableOptional,
    UnavailableError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub state: CapabilityState,
    pub optional: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

impl Capability {
    pub fn new(state: CapabilityState, optional: bool) -> Self {
        Self {
            state,
            optional,
            reason: None,
            remediation: None,
            retry_after_ms: None,
        }
    }
}

/// Fixed keys prevent independent surfaces from drifting in spelling or coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub symbols: Capability,
    pub call_graph: Capability,
    pub git_history: Capability,
    pub semantic_search: Capability,
}

/// Computed together so agent readiness and process exit status cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    Degraded,
    Unusable,
}

impl Readiness {
    pub fn agent_ready(self) -> bool {
        self != Self::Unusable
    }

    pub fn exit_code(self) -> i32 {
        match self {
            Self::Ready => 0,
            Self::Degraded => 1,
            Self::Unusable => 2,
        }
    }
}

impl Capabilities {
    /// Missing optional dependencies alone do not degrade an otherwise ready agent.
    /// Required warming/error/absence or an unhealthy transport are unusable.
    pub fn readiness(&self, transport_healthy: bool) -> Readiness {
        use CapabilityState::*;
        if !transport_healthy {
            return Readiness::Unusable;
        }
        let mut result = Readiness::Ready;
        for capability in [
            &self.symbols,
            &self.call_graph,
            &self.git_history,
            &self.semantic_search,
        ] {
            if !capability.optional && !matches!(capability.state, Ready | StaleUsable) {
                return Readiness::Unusable;
            }
            if matches!(capability.state, WarmingUp | StaleUsable | UnavailableError) {
                result = Readiness::Degraded;
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ready() -> Capabilities {
        Capabilities {
            symbols: Capability::new(CapabilityState::Ready, false),
            call_graph: Capability::new(CapabilityState::Ready, false),
            git_history: Capability::new(CapabilityState::Ready, false),
            semantic_search: Capability::new(CapabilityState::Ready, true),
        }
    }

    #[test]
    fn all_state_combinations_follow_the_exit_contract() {
        use CapabilityState::*;
        let states = [
            Ready,
            WarmingUp,
            StaleUsable,
            UnavailableOptional,
            UnavailableError,
        ];
        // Exercise each required capability independently and all combinations,
        // including simultaneous required and optional failures.
        for symbols in states {
            for calls in states {
                for git in states {
                    for semantic in states {
                        let capabilities = Capabilities {
                            symbols: Capability::new(symbols, false),
                            call_graph: Capability::new(calls, false),
                            git_history: Capability::new(git, false),
                            semantic_search: Capability::new(semantic, true),
                        };
                        let required = [symbols, calls, git];
                        let usable = required.iter().all(|s| [Ready, StaleUsable].contains(s));
                        let current = required == [Ready; 3]
                            && [Ready, UnavailableOptional].contains(&semantic);
                        let expected_code = if !usable {
                            2
                        } else if current {
                            0
                        } else {
                            1
                        };
                        let actual = capabilities.readiness(true);
                        assert_eq!(actual.agent_ready(), usable, "{capabilities:?}");
                        assert_eq!(actual.exit_code(), expected_code, "{capabilities:?}");
                        assert_eq!(capabilities.readiness(false), Readiness::Unusable);
                    }
                }
            }
        }
    }

    #[test]
    fn every_state_has_a_stable_wire_name() {
        use CapabilityState::*;
        for (state, name) in [
            (Ready, "ready"),
            (WarmingUp, "warming_up"),
            (StaleUsable, "stale_usable"),
            (UnavailableOptional, "unavailable_optional"),
            (UnavailableError, "unavailable_error"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), json!(name));
            assert_eq!(
                serde_json::from_value::<CapabilityState>(json!(name)).unwrap(),
                state
            );
        }
    }

    #[test]
    fn wire_contract_uses_fixed_keys_and_omits_absent_diagnostics() {
        let mut capabilities = ready();
        capabilities.semantic_search = Capability::new(CapabilityState::UnavailableOptional, true);
        let expected = json!({
            "symbols": {"state": "ready", "optional": false},
            "call_graph": {"state": "ready", "optional": false},
            "git_history": {"state": "ready", "optional": false},
            "semantic_search": {"state": "unavailable_optional", "optional": true}
        });
        assert_eq!(serde_json::to_value(&capabilities).unwrap(), expected);
        assert_eq!(
            serde_json::from_value::<Capabilities>(expected).unwrap(),
            capabilities
        );
        assert_eq!(capabilities.readiness(true), Readiness::Ready);
    }

    #[test]
    fn diagnostics_round_trip_and_unknown_fields_are_tolerated() {
        let value = json!({
            "state": "warming_up", "optional": false,
            "reason": "Indexing repository", "remediation": "Retry after indexing",
            "retry_after_ms": 3000, "future_field": true
        });
        let capability: Capability = serde_json::from_value(value).unwrap();
        let serialized = serde_json::to_value(&capability).unwrap();
        assert_eq!(serialized["retry_after_ms"], 3000);
        assert_eq!(serialized["reason"], "Indexing repository");
        assert_eq!(serialized["remediation"], "Retry after indexing");
        assert!(serialized.get("future_field").is_none());
        assert_eq!(
            serde_json::from_value::<Capability>(serialized).unwrap(),
            capability
        );
        assert!(serde_json::from_value::<Capability>(json!({"state": "ready"})).is_err());
        assert!(serde_json::from_value::<Capability>(
            json!({"state": "unknown", "optional": false})
        )
        .is_err());
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;
    use serde_json::json;

    fn warming(phase: IndexPhase) -> IndexLifecycleSnapshot {
        IndexLifecycleSnapshot {
            sequence: 7,
            attempt_id: 1,
            state: IndexState::WarmingUp,
            phase,
            started_at_unix_ms: 1_000,
            completed_at_unix_ms: None,
            target_commit: Some("abc123".into()),
            indexed_commit: None,
            files_total: Some(1100),
            files_completed: 420,
            files_failed: 0,
            retry_after_ms: Some(3000),
            problem: None,
            warnings: Vec::new(),
        }
    }

    fn ready_snapshot() -> IndexLifecycleSnapshot {
        let mut snapshot = warming(IndexPhase::Overlay);
        snapshot.state = IndexState::Ready;
        snapshot.indexed_commit = Some("abc123".into());
        snapshot.completed_at_unix_ms = Some(2_000);
        snapshot.retry_after_ms = None;
        snapshot
    }

    fn failed_snapshot() -> IndexLifecycleSnapshot {
        let mut snapshot = warming(IndexPhase::Scanning);
        snapshot.state = IndexState::UnavailableError;
        snapshot.completed_at_unix_ms = Some(2_000);
        snapshot.retry_after_ms = None;
        snapshot.problem = Some(Problem {
            code: "index_failed".into(),
            message: "boom".into(),
            remediation: "retry".into(),
            retryable: true,
        });
        snapshot
    }

    #[test]
    fn graph_independent_tools_are_never_gated() {
        for state_snapshot in [
            warming(IndexPhase::Discovering),
            failed_snapshot(),
            ready_snapshot(),
        ] {
            assert!(gate_tool_call("get_health", &state_snapshot, true).is_none());
            assert!(gate_tool_call("get_capabilities", &state_snapshot, false).is_none());
        }
    }

    #[test]
    fn unknown_tool_names_fall_through_to_the_unknown_tool_path() {
        assert!(gate_tool_call("not_a_real_tool", &warming(IndexPhase::Scanning), true).is_none());
    }

    #[test]
    fn graph_required_tool_is_gated_while_warming_and_dispatches_once_ready() {
        let lifecycle = warming(IndexPhase::Scanning);
        let gated = gate_tool_call("find_anchors", &lifecycle, true).expect("must gate");
        assert_eq!(gated.state, "warming_up");
        assert_eq!(gated.capability, "symbols");
        assert!(!gated.is_error());
        assert_eq!(gated.retry_after_ms, Some(3000));
        assert_eq!(
            gated.progress.as_ref().map(|p| p.phase),
            Some(IndexPhase::Scanning)
        );

        assert!(gate_tool_call("find_anchors", &ready_snapshot(), true).is_none());
    }

    #[test]
    fn graph_required_tool_reports_a_terminal_error_with_iserror_true() {
        let gated = gate_tool_call("find_anchors", &failed_snapshot(), true).expect("must gate");
        assert_eq!(gated.state, "unavailable_error");
        assert!(gated.is_error());
        assert!(gated.problem.is_some());
        assert!(gated.progress.is_none());
    }

    #[test]
    fn semantic_required_tool_without_a_model_is_unavailable_optional_not_an_error() {
        // Even mid-indexing, absence of a model is a fixed condition, not
        // a transient warm-up state.
        let gated = gate_tool_call("semantic_search", &warming(IndexPhase::Scanning), false)
            .expect("must gate");
        assert_eq!(gated.state, "unavailable_optional");
        assert_eq!(gated.capability, "semantic_search");
        assert!(!gated.is_error());

        let gated_when_ready =
            gate_tool_call("semantic_search", &ready_snapshot(), false).expect("must gate");
        assert_eq!(gated_when_ready.state, "unavailable_optional");
    }

    #[test]
    fn semantic_required_tool_with_a_model_tracks_the_structural_lifecycle() {
        assert!(gate_tool_call("semantic_search", &ready_snapshot(), true).is_none());

        let gated = gate_tool_call("semantic_search", &warming(IndexPhase::Enriching), true)
            .expect("must gate");
        assert_eq!(gated.state, "warming_up");
        assert_eq!(gated.capability, "semantic_search");

        let gated_error =
            gate_tool_call("semantic_search", &failed_snapshot(), true).expect("must gate");
        assert_eq!(gated_error.state, "unavailable_error");
        assert!(gated_error.is_error());
    }

    #[test]
    fn warming_up_envelope_matches_the_documented_wire_shape() {
        let gated = GatedResponse::warming_up(&warming(IndexPhase::Scanning), "symbols");
        let value = serde_json::to_value(&gated).unwrap();
        assert_eq!(
            value,
            json!({
                "schema_version": 1,
                "attempt_id": 1,
                "sequence": 7,
                "state": "warming_up",
                "capability": "symbols",
                "message": "LAIN is indexing this repository.",
                "progress": {
                    "phase": "scanning",
                    "files_completed": 420,
                    "files_total": 1100,
                    "files_failed": 0
                },
                "retry_after_ms": 3000,
                "next_action": {
                    "tool": "get_capabilities",
                    "arguments": {}
                }
            })
        );
        // Field order is part of the byte-stable contract the roadmap asks
        // for: confirm the serializer emits keys in declaration order.
        let text = serde_json::to_string(&gated).unwrap();
        let key_order: Vec<&str> = [
            "schema_version",
            "attempt_id",
            "sequence",
            "state",
            "capability",
            "message",
            "progress",
            "retry_after_ms",
            "next_action",
        ]
        .into_iter()
        .filter(|k| text.contains(&format!("\"{k}\"")))
        .collect();
        let positions: Vec<usize> = key_order
            .iter()
            .map(|k| text.find(&format!("\"{k}\"")).unwrap())
            .collect();
        let mut sorted = positions.clone();
        sorted.sort_unstable();
        assert_eq!(
            positions, sorted,
            "key order drifted from declaration order: {text}"
        );
    }

    #[test]
    fn unavailable_error_envelope_carries_the_problem_and_no_progress() {
        let gated = GatedResponse::unavailable_error(&failed_snapshot(), "symbols");
        let value = serde_json::to_value(&gated).unwrap();
        assert_eq!(value["state"], "unavailable_error");
        assert!(value.get("progress").is_none());
        assert!(value.get("retry_after_ms").is_none());
        assert_eq!(value["problem"]["code"], "index_failed");
        assert!(gated.is_error());
    }

    #[test]
    fn blocking_repos_is_empty_and_omitted_for_a_single_workspace_response() {
        for gated in [
            GatedResponse::warming_up(&warming(IndexPhase::Scanning), "symbols"),
            GatedResponse::unavailable_error(&failed_snapshot(), "symbols"),
            GatedResponse::unavailable_optional(&warming(IndexPhase::Scanning)),
        ] {
            assert!(gated.blocking_repos.is_empty());
            let value = serde_json::to_value(&gated).unwrap();
            assert!(
                value.get("blocking_repos").is_none(),
                "blocking_repos must be omitted from the wire shape when empty: {value}"
            );
        }
    }

    #[test]
    fn blocking_repos_serializes_sorted_when_populated() {
        let mut gated = GatedResponse::warming_up(&warming(IndexPhase::Scanning), "symbols");
        gated.blocking_repos = vec!["repo-a".into(), "repo-b".into()];
        let value = serde_json::to_value(&gated).unwrap();
        assert_eq!(value["blocking_repos"], json!(["repo-a", "repo-b"]));
    }
}
