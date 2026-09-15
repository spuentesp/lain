//! Federation-aware wrapper around the core readiness gate
//! (`crate::server::readiness`). AGENT_UX_ROADMAP.md Milestone 4 step 8:
//! each repository in a federation owns its own `RepoHealth`, and a tool
//! call must gate on the repo(s) it actually targets, not the one
//! process-global `ReadinessHandle` the single-workspace pipeline uses.
//!
//! This module does not duplicate `GatedResponse`'s wire-shape
//! construction: it maps each repo's `RepoHealth` into the same
//! `IndexLifecycleSnapshot` shape the single-workspace coordinator
//! reports, then reuses `gate_tool_call` verbatim, once per repo.

use super::health::RepoHealth;
use super::repo_id::RepoId;
use crate::server::readiness::{
    gate_tool_call, GatedResponse, IndexLifecycleSnapshot, IndexState, Problem,
};

/// `RepoIndex` has no phase/file-progress instrumentation (unlike the
/// single-workspace `build_core_memory` coordinator), so this mapping is
/// necessarily coarser than the single-workspace lifecycle: `Indexing`
/// becomes a bare `warming_up` with no progress detail, and every
/// unhealthy variant collapses to one terminal `unavailable_error` with a
/// problem code naming which `RepoHealth` variant caused it.
fn repo_health_to_snapshot(health: RepoHealth) -> IndexLifecycleSnapshot {
    let mut snapshot = IndexLifecycleSnapshot::warming_up();
    match health {
        RepoHealth::Ready => {
            snapshot.state = IndexState::Ready;
            snapshot.retry_after_ms = None;
        }
        RepoHealth::Indexing => {
            // Defaults from `warming_up()` already match: WarmingUp,
            // no progress detail.
        }
        RepoHealth::Degraded | RepoHealth::Unavailable | RepoHealth::Missing => {
            let code = match health {
                RepoHealth::Degraded => "repo_degraded",
                RepoHealth::Unavailable => "repo_unavailable",
                RepoHealth::Missing => "repo_missing",
                RepoHealth::Ready | RepoHealth::Indexing => unreachable!(),
            };
            snapshot.state = IndexState::UnavailableError;
            snapshot.retry_after_ms = None;
            snapshot.problem = Some(Problem {
                code: code.into(),
                message: format!("Repository is {health}"),
                remediation: "Run `get_federation_health` for per-repository detail.".into(),
                retryable: true,
            });
        }
    }
    snapshot
}

/// Federation-mode analogue of `crate::server::readiness::gate_tool_call`:
/// gates a tool call against every repo in its resolved input set instead
/// of a single process-global handle. `resolved` need not be pre-sorted —
/// the `blocking_repos` list on the returned response always is, so the
/// same unhealthy repo set produces byte-identical output regardless of
/// `FederatedIndex::list_repos`'s internal iteration order.
pub fn gate_federated_tool_call(
    name: &str,
    resolved: &[(RepoId, RepoHealth)],
    semantic_model_configured: bool,
) -> Option<GatedResponse> {
    let mut sorted: Vec<(RepoId, RepoHealth)> = resolved.to_vec();
    sorted.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));

    let mut blocking: Vec<(String, GatedResponse)> = Vec::new();
    for (id, health) in &sorted {
        let snapshot = repo_health_to_snapshot(*health);
        if let Some(gated) = gate_tool_call(name, &snapshot, semantic_model_configured) {
            blocking.push((id.as_str().to_string(), gated));
        }
    }

    if blocking.is_empty() {
        return None;
    }

    // A terminal failure outranks a transient wait: if any blocking repo
    // is a hard error, that becomes the representative state/message;
    // otherwise the first (sorted) blocking repo's response.
    let representative_idx = blocking
        .iter()
        .position(|(_, g)| g.state == "unavailable_error")
        .unwrap_or(0);
    let mut response = blocking[representative_idx].1.clone();
    response.blocking_repos = blocking.iter().map(|(id, _)| id.clone()).collect();
    Some(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(id: &str, health: RepoHealth) -> (RepoId, RepoHealth) {
        (RepoId::new(id).unwrap(), health)
    }

    #[test]
    fn all_ready_gates_nothing() {
        let resolved = vec![repo("b", RepoHealth::Ready), repo("a", RepoHealth::Ready)];
        assert!(gate_federated_tool_call("find_anchors", &resolved, true).is_none());
    }

    #[test]
    fn blocking_repos_is_sorted_regardless_of_input_order() {
        let resolved = vec![
            repo("zeta", RepoHealth::Ready),
            repo("beta", RepoHealth::Indexing),
            repo("alpha", RepoHealth::Unavailable),
        ];
        let gated = gate_federated_tool_call("find_anchors", &resolved, true).expect("must gate");
        assert_eq!(gated.blocking_repos, vec!["alpha", "beta"]);
    }

    #[test]
    fn a_terminal_failure_outranks_a_transient_wait_in_the_representative_state() {
        let resolved = vec![
            repo("alpha", RepoHealth::Indexing),
            repo("beta", RepoHealth::Unavailable),
        ];
        let gated = gate_federated_tool_call("find_anchors", &resolved, true).expect("must gate");
        assert_eq!(gated.state, "unavailable_error");
        assert!(gated.is_error());
        assert_eq!(gated.blocking_repos, vec!["alpha", "beta"]);
    }

    #[test]
    fn warming_up_when_only_indexing_repos_block() {
        let resolved = vec![repo("a", RepoHealth::Ready), repo("b", RepoHealth::Indexing)];
        let gated = gate_federated_tool_call("find_anchors", &resolved, true).expect("must gate");
        assert_eq!(gated.state, "warming_up");
        assert_eq!(gated.blocking_repos, vec!["b"]);
    }

    #[test]
    fn graph_independent_tools_are_never_gated_regardless_of_repo_health() {
        let resolved = vec![repo("a", RepoHealth::Unavailable)];
        assert!(gate_federated_tool_call("get_health", &resolved, true).is_none());
    }

    #[test]
    fn semantic_required_without_a_model_is_unavailable_optional_regardless_of_health() {
        let resolved = vec![repo("a", RepoHealth::Ready), repo("b", RepoHealth::Indexing)];
        let gated = gate_federated_tool_call("semantic_search", &resolved, false)
            .expect("must gate");
        assert_eq!(gated.state, "unavailable_optional");
    }
}
