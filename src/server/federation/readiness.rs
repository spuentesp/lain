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
use serde::{Deserialize, Serialize};

/// Per-repository readiness snapshot. Fed verbatim into the per-repo entry
/// of `get_capabilities` so a caller can decide whether to wait, fail, or
/// proceed — see `FederatedIndex::per_repo_readiness` for the snapshot
/// function and `get_capabilities` for the wire shape.
///
/// `Staleness` reuses the existing [`crate::server::readiness::CapabilityState`]
/// vocabulary (the "no parallel readiness state model" rule in
/// `docs/M4-step-8-plan.md` §7). `indexed_signal` and `outstanding_files`
/// carry the watcher-side observability `CapabilityState` does not cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerRepoReadiness {
    pub repo_id: RepoId,
    /// Per-repo health as the indexer reports it. Distinct from
    /// `staleness`: a repo can be `Ready` yet still `stale_usable`
    /// when the working tree has edits the watcher has not yet
    /// republished.
    pub state: RepoHealth,
    /// True once `RepoIndex::index` (or `index_forced`) has fired at
    /// least once on this repo. Independent of `state`: a repo
    /// whose first `index` failed is `Degraded` but `indexed_signal`
    /// stays `false`.
    pub indexed_signal: bool,
    /// HEAD commit at the time of the last successful index, when
    /// known. The string form (not `git2::Oid`) matches the rest of
    /// lain's commit-hash wire shape.
    pub last_indexed_commit: Option<String>,
    pub last_indexed_at_unix_ms: Option<u64>,
    /// Depth of the watcher's event channel at snapshot time. Currently
    /// always 0 — the AtomicUsize counter is wired up so the spawn_blocking
    /// follow-up PR can fill it in without changing this DTO. Pinning the
    /// field now keeps the wire shape stable across that work.
    pub outstanding_files: u64,
    /// Re-uses `CapabilityState` so `get_capabilities` does not need a
    /// parallel staleness vocabulary. Mapping:
    /// - `Ready`            → `Ready`
    /// - `Indexing`         → `WarmingUp`
    /// - `Degraded`/`Unavailable`/`Missing` → `UnavailableError`
    /// `StaleUsable` is reserved for the future "graph caught up but
    /// the working tree has unpushed edits" state.
    pub staleness: crate::server::readiness::CapabilityState,
}

/// `RepoIndex` has no phase/file-progress instrumentation (unlike the
/// single-workspace `build_core_memory` coordinator), so this mapping is
/// necessarily coarser than the single-workspace lifecycle: `Indexing`
/// becomes a bare `warming_up` with no progress detail, and every
/// unhealthy variant collapses to one terminal `unavailable_error` with a
/// problem code naming which `RepoHealth` variant caused it.
pub(crate) fn repo_health_to_snapshot(health: RepoHealth) -> IndexLifecycleSnapshot {
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

/// Map a `RepoHealth` to the canonical `CapabilityState` used in
/// `PerRepoReadiness::staleness` and (already) in `get_capabilities`'s
/// per-repo `capabilities.{symbols,call_graph,git_history,semantic_search}`
/// entries. `StaleUsable` and `UnavailableOptional` are reserved for
/// future states (a healthy graph that is behind working-tree edits;
/// a missing optional embedder) and never produced here today.
pub(crate) fn repo_health_to_capability_state(
    health: RepoHealth,
) -> crate::server::readiness::CapabilityState {
    use crate::server::readiness::CapabilityState;
    match health {
        RepoHealth::Ready => CapabilityState::Ready,
        RepoHealth::Indexing => CapabilityState::WarmingUp,
        RepoHealth::Degraded | RepoHealth::Unavailable | RepoHealth::Missing => {
            CapabilityState::UnavailableError
        }
    }
}

/// Build a `PerRepoReadiness` from the raw signals `FederatedIndex`
/// already holds. Centralized so `per_repo_readiness` (the snapshot)
/// and `get_capabilities` (the wire payload) cannot disagree on what
/// "ready" means.
pub(crate) fn build_per_repo_readiness(
    id: &RepoId,
    health: RepoHealth,
    indexed_signal: bool,
    last_indexed_commit: Option<String>,
    last_indexed_at_unix_ms: Option<u64>,
    outstanding_files: u64,
) -> PerRepoReadiness {
    PerRepoReadiness {
        repo_id: id.clone(),
        state: health,
        indexed_signal,
        last_indexed_commit,
        last_indexed_at_unix_ms,
        outstanding_files,
        staleness: repo_health_to_capability_state(health),
    }
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
        let resolved = vec![
            repo("a", RepoHealth::Ready),
            repo("b", RepoHealth::Indexing),
        ];
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
        let resolved = vec![
            repo("a", RepoHealth::Ready),
            repo("b", RepoHealth::Indexing),
        ];
        let gated =
            gate_federated_tool_call("semantic_search", &resolved, false).expect("must gate");
        assert_eq!(gated.state, "unavailable_optional");
    }
}
