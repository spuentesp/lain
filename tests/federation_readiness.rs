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
//!
//! The bottom section adds one end-to-end readiness-truthfulness pin
//! (`call_graph_ready_implies_call_answers_are_complete`): it boots a
//! real `lain server` over a deterministic one-caller/one-callee
//! fixture and asserts the readiness CONTRACT — whenever
//! `get_capabilities` reports `call_graph: ready`, `get_call_sites`
//! must name the real caller. It is the regression pin for the
//! premature-ready bug where the capability said `ready` while a
//! re-index pass had torn the `Calls` edges down and clients got
//! false "leaf" answers.

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
    // Acceptance criterion for docs/FOLLOWUPS.md entry #6:
    // `last_indexed_commit` is non-null exactly when a successful
    // index pass has reached a commit. Pinning the concrete git
    // hash would tie the test to the fixture's commit, which is
    // already created by `new_git_repo`. A non-empty, hex-looking
    // string is enough to prove the gate flipped.
    assert!(
        r1.last_indexed_commit.is_some(),
        "after a successful index, last_indexed_commit must be Some, got {:?}",
        r1.last_indexed_commit
    );
}

#[tokio::test]
async fn per_repo_readiness_nulls_last_indexed_commit_until_index_runs() {
    // Cold-repo wire shape: between `add_repo` and the first
    // successful `index_forced()`, `last_indexed_commit` and
    // `last_indexed_at_unix_ms` must be `None`. Before the fix
    // the disk-hydrated `last_commit` leaked through and
    // `get_capabilities.repositories[].last_indexed_commit`
    // surfaced a value that the in-process graph had not yet
    // reached — see docs/FOLLOWUPS.md entry #6.
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
    let src_dir = new_git_repo();
    let src: Box<dyn lain::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("r").unwrap(), src_dir.path().to_path_buf()).unwrap(),
    );
    fed.add_repo(src, tmp.path()).await.unwrap();

    // No indexing yet. The wire contract must report null commit
    // and null timestamp regardless of what the persisted
    // graph.bin happens to say.
    let r0 = &fed.per_repo_readiness()[0];
    assert!(
        !r0.indexed_signal,
        "indexed_signal must start false on a freshly added repo"
    );
    assert!(
        r0.last_indexed_commit.is_none(),
        "cold repo last_indexed_commit must be None until indexed_signal flips true, got {:?}",
        r0.last_indexed_commit
    );
    assert!(
        r0.last_indexed_at_unix_ms.is_none(),
        "cold repo last_indexed_at_unix_ms must be None until indexed_signal flips true, got {:?}",
        r0.last_indexed_at_unix_ms
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

// ── Readiness truthfulness: `call_graph` vs. the served call answers ────────
//
// End-to-end through `lain server --config` (the exact shape the demo /
// acceptance controller polls). Deterministic fixture: one caller/callee
// pair, `entry` calls `callee`.

#[path = "common/mod.rs"]
#[allow(clippy::duplicate_mod)]
mod common;

/// One deterministic caller/callee pair in a committed git repo:
/// `entry` calls `callee`, so `get_call_sites("callee")` must name
/// `entry` once the call graph for the indexed commit is served.
fn truthfulness_fixture(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"call_graph_truth\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(root.join("src/lib.rs"), truthfulness_lib_source()).unwrap();
    common::git_init_committed(root);
}

fn truthfulness_lib_source() -> String {
    let mut src = String::from(
        "pub fn entry() -> u32 { callee(1) + 2 }\n\
         pub fn callee(x: u32) -> u32 { x + 1 }\n\
         pub fn pad_target(x: u32) -> u32 { x + 1 }\n",
    );
    // Enough symbols and call edges that a pure tree-sitter re-scan (e.g.
    // Windows CI where rust-analyzer is not preinstalled) stays in the
    // `clear_calls_published() .. mark_calls_published()` window across
    // multiple 256-node ingest batches instead of finishing in sub-ms
    // time between two loopback HTTP polls.
    for i in 0..600 {
        src.push_str(&format!(
            "pub fn pad_{i:03}(x: u32) -> u32 {{ pad_target(x) + {i} }}\n"
        ));
    }
    src
}

fn caps_json(host: &str) -> serde_json::Value {
    let text = common::tools_call_text(host, "get_capabilities", serde_json::json!({}));
    serde_json::from_str(&text).unwrap_or_default()
}

fn cap_state(caps: &serde_json::Value, name: &str) -> String {
    caps["capabilities"][name]["state"]
        .as_str()
        .unwrap_or("?")
        .to_string()
}

fn call_sites(host: &str, symbol: &str) -> String {
    common::tools_call_text(
        host,
        "get_call_sites",
        serde_json::json!({ "symbol": symbol }),
    )
}

/// Readiness CONTRACT (verdict soundness): when `get_capabilities`
/// reports `symbols: ready` AND `call_graph: ready`, `get_call_sites`
/// on a known pair must name the real caller — never a false "leaf".
///
/// Cold boot on the fixture, then the same invariant is polled across a
/// watcher-driven re-index. `index_forced` replaces nodes in place
/// (taking their incident `Calls` edges with them) and only re-inserts
/// them in its resolve phase; the capability must not claim `ready`
/// inside that window. Before the fix it did — clients querying at the
/// ready-transition saw "callee is a leaf" while `index_one_repo`'s
/// caller was demonstrably in the tree — and `call_graph` never left
/// `ready` during the rebuild at all.
#[test]
fn call_graph_ready_implies_call_answers_are_complete() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = tmp.path().join("fixture");
    truthfulness_fixture(&fixture);

    let fed = tmp.path().join("fed");
    std::fs::create_dir_all(fed.join("data")).unwrap();
    let repos_yaml = fed.join("repos.yaml");
    std::fs::write(
        &repos_yaml,
        format!(
            "data_dir: {}\nrepos:\n  - id: truth\n    source: {{ type: workspace_dir, path: {} }}\n",
            fed.join("data").display(),
            fixture.display(),
        ),
    )
    .unwrap();

    let (host, mut guard) = common::boot_and_wait(&repos_yaml);

    // Cold-boot gate: wait for BOTH capabilities to report `ready` —
    // the exact contract the demo / acceptance controller polls.
    let start = std::time::Instant::now();
    let caps = loop {
        let caps = caps_json(&host);
        if cap_state(&caps, "symbols") == "ready" && cap_state(&caps, "call_graph") == "ready" {
            break caps;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(120),
            "index never reported both capabilities ready; caps: {caps}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    };

    // AUDIT (git_history): the capability builder hardcodes
    // `git_history: Ready` — it never reflects a probe. Pin the
    // observation on the same fixture: it reports `ready` at the
    // ready-transition (and at every later observation below).
    assert_eq!(
        cap_state(&caps, "git_history"),
        "ready",
        "audit: git_history is hardcoded ready in the capability builder; \
         got {}",
        cap_state(&caps, "git_history")
    );

    // The readiness CONTRACT at the ready-transition.
    let out = call_sites(&host, "callee");
    assert!(
        out.contains("entry"),
        "get_capabilities reported `call_graph: ready` but get_call_sites('callee') \
         did not name its caller `entry`: {out}"
    );

    // Same contract across a watcher-driven re-index: an uncommitted
    // edit disables `index_forced`'s cycle suppression, so the next
    // notify event re-scans the tree — the window in which the graph
    // is half-built (nodes replaced, `Calls` edges not yet re-inserted).
    let mut touch_seq = 0usize;
    let touch_worktree = |seq: usize| {
        std::fs::write(
            fixture.join("src/lib.rs"),
            format!(
                "{}// touched: force a watcher re-index #{seq}\n",
                truthfulness_lib_source()
            ),
        )
        .unwrap();
    };
    touch_worktree(touch_seq);
    let mut last_touch = std::time::Instant::now();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut violations: Vec<String> = Vec::new();
    let mut saw_rebuild = false;
    let mut symbols_ready_while_rebuilding = 0usize;
    while std::time::Instant::now() < deadline {
        let caps = caps_json(&host);
        let s = cap_state(&caps, "symbols");
        let g = cap_state(&caps, "call_graph");
        if g == "ready" {
            let out = call_sites(&host, "callee");
            if !out.contains("entry") {
                violations.push(format!("symbols={s} call_graph={g}: {}", out.trim()));
            } else if saw_rebuild {
                // Rebuild signalled honestly and is complete again.
                break;
            } else if last_touch.elapsed() >= std::time::Duration::from_millis(400) {
                touch_seq += 1;
                touch_worktree(touch_seq);
                last_touch = std::time::Instant::now();
            }
        } else {
            saw_rebuild = true;
            // AUDIT (symbols): `symbols` keeps its pre-existing
            // semantics (structural state only) through a rebuild —
            // the fix must not regress it. Documented here so a
            // change to that mapping is visible; the report notes
            // symbol answers can themselves be mid-teardown.
            if s == "ready" {
                symbols_ready_while_rebuilding += 1;
            }
        }
    }
    assert!(
        violations.is_empty(),
        "call_graph reported `ready` while the call graph was being rebuilt \
         and get_call_sites answered a false leaf: {violations:?}"
    );
    assert!(
        saw_rebuild,
        "the re-index window was never observed: `call_graph` must leave `ready` \
         while call edges are being rebuilt (readiness honesty)"
    );
    // AUDIT (symbols) — one assert: `symbols` keeps reporting `ready`
    // through the rebuild (its mapping is structural-only and is
    // deliberately unchanged by this fix — "do not regress symbols
    // semantics"). The report notes the audit finding: symbol sets are
    // also mid-teardown during the rebuild, the same premature-ready
    // class, left for a follow-up.
    assert!(
        symbols_ready_while_rebuilding > 0,
        "audit: `symbols` must keep its structural-only semantics (ready) \
         through a rebuild — this fix must not change the symbols mapping"
    );
    let _ = guard.exit_diag();
}
