//! Shared wire types and readiness policy for CLI and MCP diagnostics.
//!
//! This module performs no probes or I/O. Callers must supply observed capability
//! and transport state; a capability must never be marked ready by default.

use serde::{Deserialize, Serialize};

/// Increment only for incompatible changes; consumers may ignore added fields.
pub const SCHEMA_VERSION: u32 = 1;

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
