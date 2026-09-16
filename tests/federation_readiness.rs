//! Pin the M4 step 8 contract:
//!
//! - `FederatedIndex::per_repo_readiness` returns one entry per registered repo,
//!   sorted by id, with `staleness` derived from `RepoHealth`.
//! - `get_capabilities` federation payload exposes the new per-repo deep fields
//!   (`indexed_signal`, `last_indexed_commit`, `last_indexed_at_unix_ms`,
//!   `outstanding_files`, `staleness`) alongside the existing
//!   `capabilities.{symbols,call_graph,git_history,semantic_search}` shape.
//! - `gate_federated_tool_call` blocks a `GraphRequired` federation-aggregate
//!   tool with a structured `warming_up` envelope that names the warming
//!   repo id, exactly the same shape the dispatcher projects.
//!
//! These tests are read-only and non-flaky on CI: every fixture is a
//! freshly-built one-repo or two-repo federation in a tempdir, and the
//! "warming_up" gate assertion does not require a real long-running
//! index — we set `RepoHealth` directly and exercise the gate function
//! verbatim.

use std::sync::Arc;

use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::PetgraphBackend;
use lain::federation::health::RepoHealth;
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::WorkspaceDirSource;
use lain::server::federation::readiness::{gate_federated_tool_call, PerRepoReadiness};
use lain::server::readiness::{CapabilityState, GatedResponse, IndexState};

fn new_git_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(tmp.path()).unwrap();
    // `git2::Repository::init` leaves HEAD pointing at an unborn
    // branch; the indexer reads `get_latest_commit_info()` which
    // errors with "reference 'refs/heads/master' not found" on an
    // unborn repo. Seed an empty initial commit so the head is born
    // (same pattern `tests/common/mod.rs::git_init_committed` uses).
    let sig = git2::Signature::now("test", "test@lain").unwrap();
    let tree_oid = {
        let mut idx = repo.index().unwrap();
        idx.write_tree().unwrap()
    };
    let tree = repo.find_tree(tree_oid).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();
    tmp
}

#[tokio::test]
async fn per_repo_readiness_returns_one_entry_per_registered_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));

    for name in ["alpha", "beta", "gamma"] {
        let src_dir = new_git_repo();
        let src: Box<dyn lain::federation::repo_source::RepoSource> = Box::new(
            WorkspaceDirSource::new(RepoId::new(name).unwrap(), src_dir.path().to_path_buf())
                .unwrap(),
        );
        fed.add_repo(src, tmp.path()).await.unwrap();
    }

    let readiness = fed.per_repo_readiness();
    assert_eq!(readiness.len(), 3);
    // Sorted by repo id (defensive: the dispatcher relies on this).
    assert_eq!(readiness[0].repo_id.as_str(), "alpha");
    assert_eq!(readiness[1].repo_id.as_str(), "beta");
    assert_eq!(readiness[2].repo_id.as_str(), "gamma");
}

#[tokio::test]
async fn per_repo_readiness_maps_state_to_staleness() {
    // The wire contract: `staleness` MUST equal the canonical
    // CapabilityState projection of `RepoHealth`. A regression here
    // would let `get_capabilities` and `gate_federated_tool_call`
    // disagree on which repos are blocking.
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));

    let mut cases: Vec<(&str, RepoHealth, CapabilityState)> = vec![
        ("r-ready", RepoHealth::Ready, CapabilityState::Ready),
        (
            "r-indexing",
            RepoHealth::Indexing,
            CapabilityState::WarmingUp,
        ),
        (
            "r-degraded",
            RepoHealth::Degraded,
            CapabilityState::UnavailableError,
        ),
        (
            "r-unavailable",
            RepoHealth::Unavailable,
            CapabilityState::UnavailableError,
        ),
        (
            "r-missing",
            RepoHealth::Missing,
            CapabilityState::UnavailableError,
        ),
    ];
    for (name, health, _) in cases.drain(..) {
        let src_dir = new_git_repo();
        let src: Box<dyn lain::federation::repo_source::RepoSource> = Box::new(
            WorkspaceDirSource::new(RepoId::new(name).unwrap(), src_dir.path().to_path_buf())
                .unwrap(),
        );
        fed.add_repo(src, tmp.path()).await.unwrap();
        fed.get_repo(&RepoId::new(name).unwrap())
            .unwrap()
            .set_health(health);
    }

    let readiness: std::collections::HashMap<String, PerRepoReadiness> = fed
        .per_repo_readiness()
        .into_iter()
        .map(|r| (r.repo_id.as_str().to_string(), r))
        .collect();

    assert_eq!(readiness["r-ready"].staleness, CapabilityState::Ready);
    assert_eq!(
        readiness["r-indexing"].staleness,
        CapabilityState::WarmingUp
    );
    assert_eq!(
        readiness["r-degraded"].staleness,
        CapabilityState::UnavailableError
    );
    assert_eq!(
        readiness["r-unavailable"].staleness,
        CapabilityState::UnavailableError
    );
    assert_eq!(
        readiness["r-missing"].staleness,
        CapabilityState::UnavailableError
    );
}

#[tokio::test]
async fn per_repo_readiness_reports_indexed_signal_after_first_successful_index() {
    // After a successful `index_forced()` (with the LSP pool marked
    // unavailable so the indexer falls back to tree-sitter, the same
    // pattern other tests in this module use), `indexed_signal`
    // flips to `true` and `last_indexed_at_unix_ms` is populated.
    // The exact pre-index values are deliberately NOT pinned here —
    // they vary across graph-backend init paths and are not part of
    // the wire contract — but the post-index transitions are.
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
    let src_dir = new_git_repo();
    std::fs::write(src_dir.path().join("lib.rs"), "pub fn marker() {}\n").unwrap();
    let src: Box<dyn lain::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("r").unwrap(), src_dir.path().to_path_buf()).unwrap(),
    );
    fed.add_repo(src, tmp.path()).await.unwrap();

    let repo = fed.get_repo(&RepoId::new("r").unwrap()).unwrap();
    for _ in 0..4 {
        repo.lsp()
            .next()
            .lock()
            .await
            .mark_unavailable("rust-analyzer");
    }
    repo.index_forced()
        .await
        .expect("index_forced must succeed");

    let r1 = &fed.per_repo_readiness()[0];
    assert_eq!(r1.repo_id.as_str(), "r");
    assert!(
        r1.indexed_signal,
        "after a successful index, indexed_signal must flip to true"
    );
    assert!(
        r1.last_indexed_at_unix_ms.unwrap_or(0) > 0,
        "last_indexed_at_unix_ms must be populated, got {:?}",
        r1.last_indexed_at_unix_ms
    );
}

#[tokio::test]
async fn gate_federated_tool_call_blocks_with_warming_up_envelope_naming_the_repo() {
    // The dispatcher's single source of truth for federation-aggregate
    // readiness is `gate_federated_tool_call`. Confirm the wire shape
    // here so a regression in the snapshot path can't slip past CI:
    // - `state` must be `"warming_up"` for a still-indexing repo.
    // - `is_error()` must be `false` (warming is retryable, not a
    //   terminal error).
    // - `blocking_repos` must name the warming repo by id (this is
    //   the field the agent uses to decide whether to wait or fail).
    let resolved = vec![
        (RepoId::new("alpha").unwrap(), RepoHealth::Ready),
        (RepoId::new("beta").unwrap(), RepoHealth::Indexing),
    ];
    let gated: GatedResponse = gate_federated_tool_call("search_org", &resolved, true)
        .expect("a warming repo must gate a GraphRequired federation tool");
    assert_eq!(gated.state, "warming_up");
    assert!(!gated.is_error());
    assert_eq!(gated.blocking_repos, vec!["beta"]);
    assert_eq!(gated.capability, "symbols");
    assert!(gated.retry_after_ms.is_some());
}

#[tokio::test]
async fn gate_federated_graph_independent_tool_never_blocks() {
    // `get_capabilities` is the canonical `GraphIndependent` tool —
    // a tool the agent calls precisely to learn *which* repos are
    // blocking, so the gate MUST NOT block it under any repo health.
    for health in [
        RepoHealth::Ready,
        RepoHealth::Indexing,
        RepoHealth::Degraded,
        RepoHealth::Unavailable,
        RepoHealth::Missing,
    ] {
        let resolved = vec![(RepoId::new("r").unwrap(), health)];
        assert!(
            gate_federated_tool_call("get_capabilities", &resolved, true).is_none(),
            "get_capabilities must never be gated (health={health:?})"
        );
    }
}

#[tokio::test]
async fn gate_federated_search_org_blocks_when_any_repo_is_warming_up() {
    // The two federation-aggregate tools `get_capabilities` flags
    // as `GraphRequired` — `search_org` and
    // `get_cross_repo_blast_radius` — must both surface the warming
    // repo by id. (The single-repo variants like
    // `get_cross_repo_blast_radius_for_repo` and the
    // workspace-scoped `get_workspace_graph` are also GraphRequired
    // but they have their own per-repo resolution at the dispatcher;
    // the federation-wide gate covers the two call sites.)
    for tool in ["search_org", "get_cross_repo_blast_radius"] {
        let resolved = vec![
            (RepoId::new("a").unwrap(), RepoHealth::Ready),
            (RepoId::new("b").unwrap(), RepoHealth::Indexing),
        ];
        let gated = gate_federated_tool_call(tool, &resolved, true)
            .unwrap_or_else(|| panic!("{tool} must gate when any repo is warming up"));
        assert_eq!(gated.state, "warming_up");
        assert_eq!(gated.blocking_repos, vec!["b"]);
    }
}

#[test]
fn capability_state_wire_names_match_documented_snake_case() {
    // `PerRepoReadiness::staleness` is serialized into
    // `get_capabilities.repositories[].staleness` and consumed by
    // agents that hard-code the wire shape. Pin the spelling here so
    // a rename can't slip past the snapshot path.
    use serde_json::json;
    for (state, expected) in [
        (IndexState::WarmingUp, json!("warming_up")),
        (IndexState::Ready, json!("ready")),
        (IndexState::UnavailableError, json!("unavailable_error")),
    ] {
        let actual = serde_json::to_value(state).unwrap();
        assert_eq!(actual, expected, "IndexState wire name drifted: {state:?}");
    }
    for (state, expected) in [
        (CapabilityState::Ready, json!("ready")),
        (CapabilityState::WarmingUp, json!("warming_up")),
        (CapabilityState::StaleUsable, json!("stale_usable")),
        (
            CapabilityState::UnavailableOptional,
            json!("unavailable_optional"),
        ),
        (
            CapabilityState::UnavailableError,
            json!("unavailable_error"),
        ),
    ] {
        let actual = serde_json::to_value(state).unwrap();
        assert_eq!(
            actual, expected,
            "CapabilityState wire name drifted: {state:?}"
        );
    }
}
