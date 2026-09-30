//! Shared §15.1 fixture + snapshot-manager harness for contract-tool
//! tests that need real pinned snapshot pairs (`diff_contracts`
//! scenarios, `resolve_evidence` on a snapshot, stdio/HTTP parity for
//! the same base/head pair).
//!
//! Included with `#[path]` from the test files that need it (the
//! `tests/support/` pattern used by `isolated_state.rs`).
//!
//! Hermetic by construction: `scripts/contracts-fixture.sh` writes
//! four local git repos with no network, and every snapshot is pinned
//! to a 40-hex commit resolved with `git rev-parse` from the fixture's
//! own tags.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lain::federation::config::{FederationConfig, RepoConfig, SourceConfig};
use lain::federation::contracts::config::ContractFederationConfig;
use lain::federation::contracts::index_cache::IndexCache;
use lain::federation::contracts::snapshots::manager::{PrepareRequest, SnapshotManager};
use lain::federation::contracts::snapshots::{snapshot_record_path, SnapshotRecord, SnapshotState};
use lain::server::mcp::handler::{HandlerStatus, McpContext};

/// The four repos `scripts/contracts-fixture.sh` writes.
pub const FIXTURE_REPOS: [&str; 4] = ["orders", "billing", "reports", "platform"];

pub struct Fixture {
    /// Held until the test exits; the tempdir is removed on drop.
    pub _tmp: tempfile::TempDir,
    pub root: PathBuf,
}

fn fixture_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("contracts-fixture.sh")
}

/// Run `scripts/contracts-fixture.sh <tmpdir>` and keep the result
/// alive for the test.
pub fn build_fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("fixture tempdir");
    let root = tmp.path().to_path_buf();
    let status = Command::new(fixture_script())
        .arg(&root)
        .status()
        .expect("spawn contracts-fixture.sh");
    assert!(
        status.success(),
        "contracts-fixture.sh failed: exit {status:?}"
    );
    Fixture { _tmp: tmp, root }
}

/// The contract config exactly as the fixture's `repos.yaml`
/// declares it (top-level `services` / `http_clients` /
/// `generic_keys` sections — same load path as `reload.rs`).
pub fn contract_config(root: &Path) -> Arc<ContractFederationConfig> {
    Arc::new(
        ContractFederationConfig::load(&root.join("repos.yaml")).expect("fixture contract config"),
    )
}

fn data_dir(root: &Path) -> PathBuf {
    root.join(".lain-data")
}

/// The fixture `repos.yaml` as a `FederationConfig` (sources +
/// data_dir), used only to build the manager's source resolver.
fn federation_config(root: &Path) -> FederationConfig {
    let mut repos = Vec::new();
    for id in FIXTURE_REPOS {
        repos.push(RepoConfig {
            id: id.to_string(),
            source: SourceConfig::WorkspaceDir {
                path: root.join(id),
            },
        });
    }
    FederationConfig {
        data_dir: data_dir(root),
        repos,
        ..FederationConfig::default()
    }
}

/// A snapshot manager wired to the fixture's data dir and the
/// fixture repos' workspace paths.
pub fn manager(root: &Path) -> Arc<SnapshotManager> {
    let cfg = federation_config(root);
    let cache = IndexCache::new(&cfg.data_dir);
    let mgr = SnapshotManager::new(&cfg.data_dir, cache);
    let resolver = SnapshotManager::resolver_from_config(&cfg);
    mgr.set_repo_source_resolver(resolver);
    mgr
}

/// `git rev-parse --verify <rev>^{commit}` inside a fixture repo —
/// tags (`base`, `s1-remove-customer-id`, …) to 40-hex commits. The
/// snapshot pipeline keys records, cache entries and mirror tree
/// diffs by commit, so callers pin snapshots with resolved shas.
pub fn rev_parse(root: &Path, repo: &str, rev: &str) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "--verify", &format!("{rev}^{{commit}}")])
        .current_dir(root.join(repo))
        .output()
        .expect("git rev-parse");
    assert!(
        out.status.success(),
        "git rev-parse {rev} in {repo}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// `repo → sha(rev)` for every fixture repo at the same rev.
pub fn all_repos_at(root: &Path, rev: &str) -> BTreeMap<String, String> {
    FIXTURE_REPOS
        .iter()
        .map(|r| (r.to_string(), rev_parse(root, r, rev)))
        .collect()
}

/// Prepare a snapshot (optionally derived from `from`) and wait
/// until it reaches a terminal state. Returns the snapshot id;
/// panics with the per-repo states unless the snapshot is `ready`.
pub async fn prepare_ready(
    mgr: &Arc<SnapshotManager>,
    repos: BTreeMap<String, String>,
    from: Option<String>,
    config: Arc<ContractFederationConfig>,
) -> String {
    let outcome = mgr
        .prepare(PrepareRequest {
            refs_: BTreeMap::new(),
            repos,
            excluded: Vec::new(),
            from,
            max_base_age_s: None,
            wait_ms: 60_000,
            config,
        })
        .await
        .expect("prepare_snapshot");
    let id = outcome.record.id.clone();
    if matches!(outcome.record.state, SnapshotState::Failed) {
        panic!("snapshot {id} failed: {:?}", outcome.record.repo_states);
    }
    // The shared worker pool can back up under parallel test runs;
    // poll the on-disk record until the state machine settles.
    let path = snapshot_record_path(&mgr.data_dir(), &id);
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let raw = std::fs::read(&path).unwrap_or_else(|e| panic!("read {id}: {e}"));
        let rec: SnapshotRecord = serde_json::from_slice(&raw).expect("snapshot record json");
        match rec.state {
            SnapshotState::Ready => return id,
            SnapshotState::Failed => panic!("snapshot {id} failed: {:?}", rec.repo_states),
            _ => {
                if Instant::now() > deadline {
                    panic!("snapshot {id} never reached a terminal state");
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// A derived head: `from: <base>` with `repo → tag` as the override
/// (resolved to a commit first).
pub async fn derive_head(
    mgr: &Arc<SnapshotManager>,
    config: Arc<ContractFederationConfig>,
    root: &Path,
    from: &str,
    overrides: &[(&str, &str)],
) -> String {
    let mut repos = BTreeMap::new();
    for (repo, tag) in overrides {
        repos.insert(repo.to_string(), rev_parse(root, repo, tag));
    }
    prepare_ready(mgr, repos, Some(from.to_string()), config).await
}

/// The `McpContext` a snapshot-only tool call sees: no live
/// federation, the fixture's snapshot manager wired in.
pub fn snapshot_ctx<'a>(
    mgr: &'a Arc<SnapshotManager>,
    status: &'a HandlerStatus,
) -> McpContext<'a> {
    McpContext {
        server: None,
        federation: None,
        workspaces: None,
        status,
        reload_bus: None,
        snapshots: Some(mgr),
    }
}
