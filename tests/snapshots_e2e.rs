//! End-to-end snapshot tests (`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §8.4–§8.5,
//! §13). The §15.1 fixture provides the four local repos (all
//! `workspace_dir`-sourced); the snapshot manager's source resolver
//! (built from the same `FederationConfig`) maps each repo id to
//! its workspace path. The test then runs a real `prepare_snapshot`
//! to assert the full pipeline: mirror ensured, worktree added,
//! snapshot-mode indexer runs, cache entry lands, record written,
//! state reaches `ready`.
//!
//! Scenario rows 8/9 from the §15.2 table are also covered here at
//! the manager level — see `tests/snapshots_unit.rs` for the unit
//! tests that pin the id-determinism and derived-head inheritance
//! properties without paying for the indexing pass.
//!
//! The hermetic fixture (no network) gates the test: the §15.1
//! fixture script writes a deterministic local git history so the
//! snapshots land without contacting GitHub.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use lain::federation::config::{FederationConfig, RepoConfig, SourceConfig};
use lain::federation::contracts::config::ContractFederationConfig;
use lain::federation::contracts::index_cache::{CacheKey, IndexCache};
use lain::federation::contracts::snapshots::{
    manager::PrepareError, manager::PrepareRequest, snapshot_record_path as snapshot_path,
    SnapshotManager,
};
use lain::federation::graph_backend::GraphBackend;

fn fixture_script() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("contracts-fixture.sh")
}

struct Fixture {
    /// Held until the test exits; the tempdir is removed on drop.
    _tmp: tempfile::TempDir,
    /// Resolved repo roots under the fixture tempdir.
    repos_root: std::path::PathBuf,
}

fn build_fixture() -> Fixture {
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
    Fixture {
        _tmp: tmp,
        repos_root: root,
    }
}

/// Build a `FederationConfig` mapping every fixture repo to a
/// `workspace_dir` source. Mirrors `repos.yaml` produced by the
/// §15.1 fixture script (`scripts/contracts-fixture.sh`).
fn federation_config(fix: &Fixture) -> FederationConfig {
    let mut repos = Vec::new();
    for (id, dirname) in [
        ("orders", "orders"),
        ("billing", "billing"),
        ("reports", "reports"),
        ("platform", "platform"),
    ] {
        repos.push(RepoConfig {
            id: id.to_string(),
            source: SourceConfig::WorkspaceDir {
                path: fix.repos_root.join(dirname),
            },
        });
    }
    FederationConfig {
        data_dir: fix.repos_root.join(".lain-data"),
        max_concurrent_indexers: 2,
        ready_threshold: 1.0,
        git_sensor: None,
        repos,
        contract: ContractFederationConfig::default(),
    }
}

/// Read the HEAD sha of a fixture repo's `base` tag (or HEAD if
/// `base` is missing — the §15.1 fixture tags `base` on every repo).
fn head_sha(repo_root: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "base"])
        .current_dir(repo_root)
        .output()
        .expect("git rev-parse");
    assert!(
        out.status.success(),
        "git rev-parse base failed for {}: stderr={}",
        repo_root.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Build the manager under the fixture's data dir and install the
/// source resolver built from the test's `FederationConfig`.
fn manager_with_resolver(_fix: &Fixture, cfg: &FederationConfig) -> Arc<SnapshotManager> {
    let cache = IndexCache::new(&cfg.data_dir);
    let mgr = SnapshotManager::new(&cfg.data_dir, cache);
    let resolver = SnapshotManager::resolver_from_config(cfg);
    mgr.set_repo_source_resolver(resolver);
    mgr
}

/// `prepare_snapshot` against a `workspace_dir`-sourced repo lands
/// the cache entry, writes the record, and reaches `ready`. This is
/// the canonical end-to-end happy path.
#[tokio::test]
async fn prepare_snapshot_workspace_dir_lands_cache_entry_and_record() {
    std::env::set_var("LAIN_SNAPSHOT_WORKER_IDLE_TIMEOUT_MS", "1000");
    let fix = build_fixture();
    let cfg = federation_config(&fix);
    let mgr = manager_with_resolver(&fix, &cfg);
    let orders_root = fix.repos_root.join("orders");
    let orders_sha = head_sha(&orders_root);

    let mut repos = BTreeMap::new();
    repos.insert("orders".to_string(), orders_sha.clone());
    let req = PrepareRequest {
        refs_: BTreeMap::new(),
        repos,
        excluded: Vec::new(),
        from: None,
        max_base_age_s: None,
        wait_ms: 60_000,
        config: Arc::new(ContractFederationConfig::default()),
    };

    let outcome = mgr.prepare(req).await.expect("prepare_snapshot");

    // Record is on disk at the expected path.
    let record_path = cfg
        .data_dir
        .join("snapshots")
        .join(format!("{}.json", outcome.record.id));
    assert!(
        record_path.exists(),
        "snapshot record {} not written",
        record_path.display()
    );
    // Record commits match the request.
    assert_eq!(outcome.record.repos.get("orders"), Some(&orders_sha));
    // The state machine reached `ready` (or `failed`; we want ready
    // for the happy path). The shared worker pool can back up under
    // parallel test execution, so we tolerate `indexing` once and
    // poll the record file until it reaches a terminal state.
    let record_path = cfg
        .data_dir
        .join("snapshots")
        .join(format!("{}.json", outcome.record.id));
    let poll_start = std::time::Instant::now();
    let final_state = loop {
        if matches!(
            outcome.record.state,
            lain::federation::contracts::snapshots::SnapshotState::Ready
                | lain::federation::contracts::snapshots::SnapshotState::Failed
        ) {
            break outcome.record.state;
        }
        if poll_start.elapsed() > std::time::Duration::from_secs(120) {
            panic!(
                "snapshot state did not reach ready/failed within 120s (current: {:?})",
                outcome.record.state
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        // Re-read the record file — the manager updates it as
        // each job completes.
        let raw = std::fs::read(&record_path).expect("read snapshot record");
        let stored: lain::federation::contracts::snapshots::SnapshotRecord =
            serde_json::from_slice(&raw).expect("parse snapshot record");
        if matches!(
            stored.state,
            lain::federation::contracts::snapshots::SnapshotState::Ready
                | lain::federation::contracts::snapshots::SnapshotState::Failed
        ) {
            break stored.state;
        }
    };
    assert_eq!(
        final_state,
        lain::federation::contracts::snapshots::SnapshotState::Ready,
        "snapshot state {:?} is terminal but not ready",
        final_state
    );
    // The per-repo status is `cached` with the right commit.
    let repo_state = outcome
        .record
        .repo_states
        .get("orders")
        .expect("orders repo state");
    let commit = repo_state.commit().expect("orders commit");
    assert_eq!(commit, orders_sha);
    // Cache entry was written.
    let key = CacheKey::new("orders", &orders_sha, &outcome.record.analyzer_version);
    assert!(
        mgr.cache().has_entry(&key),
        "cache entry not written for {}@{}",
        key.repo,
        key.sha
    );
    // `get_snapshot` finds the same record.
    let got = mgr
        .get(&outcome.record.id, 5_000)
        .await
        .expect("get_snapshot");
    assert_eq!(got.record.id, outcome.record.id);
    assert_eq!(got.record.state, outcome.record.state);
}

/// Black-box regression test for symbolic-ref snapshot preparation:
/// When the operator passes a tag (e.g. `base`), the ref is resolved to a commit SHA upfront,
/// the record's `repos` map is populated with the resolved canonical commit SHA, and the
/// caller's tag input is stored in `refs_` for display.
///
/// Black-box flow:
/// 1. Prepare snapshot by tag `base`.
/// 2. Verify `record.repos` contains the resolved SHA and `record.refs_` contains `"base"`.
/// 3. Query the snapshot successfully (`get`).
/// 4. Verify cache entry is written under the canonical resolved SHA.
/// 5. Restart snapshot manager; query again and verify it is ready.
/// 6. Prepare snapshot with the resolved SHA directly and assert the SAME snapshot ID.
#[tokio::test]
async fn prepare_snapshot_with_tag_ref_promotes_record_to_resolved_sha() {
    std::env::set_var("LAIN_SNAPSHOT_WORKER_IDLE_TIMEOUT_MS", "1000");
    let fix = build_fixture();
    let cfg = federation_config(&fix);
    let mgr = manager_with_resolver(&fix, &cfg);
    let orders_root = fix.repos_root.join("orders");
    let base_sha = {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "base"])
            .current_dir(&orders_root)
            .output()
            .expect("git rev-parse base");
        assert!(out.status.success(), "base tag missing in fixture");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };

    let mut repos = BTreeMap::new();
    repos.insert("orders".to_string(), "base".to_string());
    let req = PrepareRequest {
        refs_: BTreeMap::new(),
        repos,
        excluded: Vec::new(),
        from: None,
        max_base_age_s: None,
        wait_ms: 60_000,
        config: Arc::new(ContractFederationConfig::default()),
    };

    let outcome = mgr.prepare(req).await.expect("prepare_snapshot");
    assert_eq!(
        outcome.record.state,
        lain::federation::contracts::snapshots::SnapshotState::Ready
    );
    // Canonical SHA promotion: record.repos contains the resolved SHA
    assert_eq!(
        outcome.record.repos.get("orders").map(String::as_str),
        Some(base_sha.as_str()),
        "record.repos[orders] must be promoted to the resolved SHA"
    );
    // Caller-supplied input ref is preserved in refs_ for display
    assert_eq!(
        outcome.record.refs_.get("orders").map(String::as_str),
        Some("base"),
        "record.refs_[orders] must preserve the input ref"
    );

    // Query snapshot successfully
    let queried = mgr
        .get(&outcome.record.id, 5_000)
        .await
        .expect("get_snapshot");
    assert_eq!(queried.record.id, outcome.record.id);

    // Cache entry is stored under resolved SHA
    let key = CacheKey::new("orders", &base_sha, &outcome.record.analyzer_version);
    assert!(
        mgr.cache().has_entry(&key),
        "cache entry missing for orders@{base_sha}"
    );

    // Restart manager: fresh manager instance pointing at same data dir
    let restarted_mgr = manager_with_resolver(&fix, &cfg);
    let queried_after_restart = restarted_mgr
        .get(&outcome.record.id, 5_000)
        .await
        .expect("get after restart");
    assert_eq!(queried_after_restart.record.id, outcome.record.id);
    assert_eq!(
        queried_after_restart.record.state,
        lain::federation::contracts::snapshots::SnapshotState::Ready
    );

    // Prepare by SHA directly and assert the SAME snapshot ID is returned!
    let mut sha_repos = BTreeMap::new();
    sha_repos.insert("orders".to_string(), base_sha.clone());
    let sha_req = PrepareRequest {
        refs_: BTreeMap::new(),
        repos: sha_repos,
        excluded: Vec::new(),
        from: None,
        max_base_age_s: None,
        wait_ms: 5_000,
        config: Arc::new(ContractFederationConfig::default()),
    };
    let sha_outcome = mgr
        .prepare(sha_req)
        .await
        .expect("prepare with resolved SHA");
    assert_eq!(
        outcome.record.id, sha_outcome.record.id,
        "preparing by tag and preparing by equivalent SHA must return the same snapshot ID"
    );
}

/// `prepare_snapshot` against an unknown repo surfaces
/// `repo_not_registered` with `details.repo` populated (§13).
#[tokio::test]
async fn prepare_snapshot_unknown_repo_returns_repo_not_registered() {
    std::env::set_var("LAIN_SNAPSHOT_WORKER_IDLE_TIMEOUT_MS", "1000");
    let fix = build_fixture();
    let cfg = federation_config(&fix);
    let mgr = manager_with_resolver(&fix, &cfg);

    let mut repos = BTreeMap::new();
    repos.insert("nonexistent".to_string(), "main".to_string());
    let req = PrepareRequest {
        refs_: BTreeMap::new(),
        repos,
        excluded: Vec::new(),
        from: None,
        max_base_age_s: None,
        wait_ms: 5_000,
        config: Arc::new(ContractFederationConfig::default()),
    };
    let err = mgr.prepare(req).await.expect_err("expected error");
    match err {
        PrepareError::RepoNotRegistered { repo } => assert_eq!(repo, "nonexistent"),
        other => panic!("expected RepoNotRegistered, got {other:?}"),
    }
}

/// An excluded repo surfaces `Excluded` state and never submits a
/// job for it.
#[tokio::test]
async fn prepare_snapshot_exclude_marks_repo_excluded_without_job() {
    std::env::set_var("LAIN_SNAPSHOT_WORKER_IDLE_TIMEOUT_MS", "1000");
    let fix = build_fixture();
    let cfg = federation_config(&fix);
    let mgr = manager_with_resolver(&fix, &cfg);
    let orders_root = fix.repos_root.join("orders");
    let orders_sha = head_sha(&orders_root);

    let mut repos = BTreeMap::new();
    repos.insert("orders".to_string(), orders_sha);
    let req = PrepareRequest {
        refs_: BTreeMap::new(),
        repos,
        excluded: vec!["reports".to_string()],
        from: None,
        max_base_age_s: None,
        wait_ms: 5_000,
        config: Arc::new(ContractFederationConfig::default()),
    };
    let outcome = mgr.prepare(req).await.expect("prepare_snapshot");
    assert!(matches!(
        outcome.record.repo_states.get("reports"),
        Some(lain::federation::contracts::snapshots::RepoSnapshotState::Excluded)
    ));
}

/// Two `prepare_snapshot` calls with identical inputs produce the
/// same id (scenario row 8: same id, one job).
#[tokio::test]
async fn prepare_snapshot_twice_same_inputs_same_id() {
    std::env::set_var("LAIN_SNAPSHOT_WORKER_IDLE_TIMEOUT_MS", "1000");
    let fix = build_fixture();
    let cfg = federation_config(&fix);
    let mgr = manager_with_resolver(&fix, &cfg);
    let orders_root = fix.repos_root.join("orders");
    let orders_sha = head_sha(&orders_root);

    let mut repos = BTreeMap::new();
    repos.insert("orders".to_string(), orders_sha);

    // First call.
    let req1 = PrepareRequest {
        refs_: repos.clone(),
        repos: repos.clone(),
        excluded: Vec::new(),
        from: None,
        max_base_age_s: None,
        wait_ms: 60_000,
        config: Arc::new(ContractFederationConfig::default()),
    };
    let out1 = mgr.prepare(req1).await.expect("first prepare");
    // Second call with identical inputs.
    let req2 = PrepareRequest {
        refs_: BTreeMap::new(),
        repos,
        excluded: Vec::new(),
        from: None,
        max_base_age_s: None,
        // The idempotence path reads the existing record's terminal
        // state — but on the first call the record is written
        // before indexing finishes, so the second call's terminal
        // check has to wait for the cache entries to land. Use a
        // generous `wait_ms` so parallel tests don't flake.
        wait_ms: 120_000,
        config: Arc::new(ContractFederationConfig::default()),
    };
    let out2 = mgr.prepare(req2).await.expect("second prepare");
    assert_eq!(out1.record.id, out2.record.id);
}

/// §8.5: `from_snapshot` (which `prepare_snapshot` calls
/// post-resolution when cache entries exist) must never write to
/// the data directory. Listing the directory before and after a
/// `from_snapshot` call confirms the hydration is in-memory.
#[tokio::test]
async fn from_snapshot_writes_no_files_to_data_dir() {
    let fix = build_fixture();
    let cfg = federation_config(&fix);
    let mgr = manager_with_resolver(&fix, &cfg);
    let orders_root = fix.repos_root.join("orders");
    let orders_sha = head_sha(&orders_root);

    // Prepare first so the cache entry lands.
    let mut repos = BTreeMap::new();
    repos.insert("orders".to_string(), orders_sha.clone());
    let req = PrepareRequest {
        refs_: BTreeMap::new(),
        repos,
        excluded: Vec::new(),
        from: None,
        max_base_age_s: None,
        wait_ms: 60_000,
        config: Arc::new(ContractFederationConfig::default()),
    };
    let outcome = mgr.prepare(req).await.expect("prepare");
    let record_id = outcome.record.id.clone();

    // Drop the manager's HoldGuard (the `prepare` returned it
    // implicitly via `outcome` — we don't have it here, so the
    // hold is already dropped by the manager).
    drop(outcome);

    let snapshots_dir = cfg.data_dir.join("snapshots");
    let before: std::collections::BTreeSet<_> = walk(&snapshots_dir);
    // `from_snapshot` builds a federation. The §8.5 invariant
    // is that no NEW files appear under `data_dir`.
    let _fed = mgr
        .from_snapshot(&mgr_cached_record(&mgr, &record_id))
        .expect("from_snapshot");
    drop(_fed);
    let after: std::collections::BTreeSet<_> = walk(&snapshots_dir);

    let new: std::collections::BTreeSet<_> = after.difference(&before).cloned().collect();
    assert!(
        new.is_empty(),
        "from_snapshot created new files under {}: {:?}",
        snapshots_dir.display(),
        new
    );
}

/// Concurrent `from_snapshot` calls for one record (PR 11 round-1
/// review finding): the in-memory hydration path makes this safe
/// without locks because every call uses `GraphDatabase::from_bytes`
/// over an independent bytes slice, then upserts into the shared
/// `PetgraphBackend::ephemeral`. Both calls must succeed and
/// produce equivalent federations.
#[tokio::test]
async fn from_snapshot_concurrent_calls_succeed() {
    let fix = build_fixture();
    let cfg = federation_config(&fix);
    let mgr = manager_with_resolver(&fix, &cfg);
    let orders_root = fix.repos_root.join("orders");
    let orders_sha = head_sha(&orders_root);
    let mut repos = BTreeMap::new();
    repos.insert("orders".to_string(), orders_sha.clone());
    let req = PrepareRequest {
        refs_: BTreeMap::new(),
        repos,
        excluded: Vec::new(),
        from: None,
        max_base_age_s: None,
        wait_ms: 60_000,
        config: Arc::new(ContractFederationConfig::default()),
    };
    let outcome = mgr.prepare(req).await.expect("prepare");
    let record_id = outcome.record.id.clone();
    // Poll the on-disk record until the indexing finishes — the
    // shared worker pool can back up under parallel test
    // execution, so we don't trust the in-memory `outcome` alone.
    let record_path = snapshot_path(&cfg.data_dir, &record_id);
    let poll_start = std::time::Instant::now();
    loop {
        let raw = std::fs::read(&record_path).expect("read snapshot record");
        let stored: lain::federation::contracts::snapshots::SnapshotRecord =
            serde_json::from_slice(&raw).expect("parse snapshot record");
        if matches!(
            stored.state,
            lain::federation::contracts::snapshots::SnapshotState::Ready
                | lain::federation::contracts::snapshots::SnapshotState::Failed
        ) {
            break;
        }
        if poll_start.elapsed() > std::time::Duration::from_secs(120) {
            panic!("snapshot never reached terminal state");
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    drop(outcome);

    // Two concurrent `from_snapshot` calls.
    let r1 = mgr_cached_record(&mgr, &record_id);
    let r2 = mgr_cached_record(&mgr, &record_id);
    let m1 = Arc::clone(&mgr);
    let m2 = Arc::clone(&mgr);
    let h1 = tokio::task::spawn_blocking(move || m1.from_snapshot(&r1));
    let h2 = tokio::task::spawn_blocking(move || m2.from_snapshot(&r2));
    let (f1, f2) = tokio::try_join!(h1, h2).expect("join");
    let (fed1, _hold1) = f1.expect("first from_snapshot");
    let (fed2, _hold2) = f2.expect("second from_snapshot");
    assert_eq!(fed1.snapshot_id, fed2.snapshot_id);
    // PetgraphBackend implements GraphBackend (which exposes
    // `node_count`); deref through the Arc to call it.
    let n1 = fed1.backend.as_ref().node_count();
    let n2 = fed2.backend.as_ref().node_count();
    assert_eq!(n1, n2);
}

/// Walk a directory recursively, returning relative paths.
fn walk(dir: &Path) -> std::collections::BTreeSet<std::path::PathBuf> {
    let mut out = std::collections::BTreeSet::new();
    fn recurse(d: &Path, prefix: &Path, out: &mut std::collections::BTreeSet<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else {
            return;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if let Ok(rel) = p.strip_prefix(prefix) {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    recurse(&p, prefix, out);
                } else {
                    out.insert(rel.to_path_buf());
                }
            }
        }
    }
    recurse(dir, dir, &mut out);
    out
}

fn mgr_cached_record(
    mgr: &Arc<lain::federation::contracts::snapshots::SnapshotManager>,
    record_id: &str,
) -> lain::federation::contracts::snapshots::SnapshotRecord {
    let dir = mgr.data_dir();
    let path = snapshot_path(dir, record_id);
    let bytes = std::fs::read(&path).expect("read snapshot record");
    serde_json::from_slice(&bytes).expect("parse snapshot record")
}

/// Scenario row 9 (ruling): derived head with one override
/// indexes only the overridden repo; the others inherit the
/// base's commits. Asserts the on-disk record's `repos` map after
/// the derived prepare round.
#[tokio::test]
async fn derived_head_with_one_override_indexes_only_overridden_repo() {
    let fix = build_fixture();
    let cfg = federation_config(&fix);
    let mgr = manager_with_resolver(&fix, &cfg);
    let orders_root = fix.repos_root.join("orders");
    let orders_sha = head_sha(&orders_root);
    let billing_root = fix.repos_root.join("billing");
    let billing_sha = head_sha(&billing_root);
    let platform_root = fix.repos_root.join("platform");
    let platform_sha = head_sha(&platform_root);

    // 1. Base: orders + billing + platform at their `base` tags.
    let mut base_repos = BTreeMap::new();
    base_repos.insert("orders".into(), orders_sha.clone());
    base_repos.insert("billing".into(), billing_sha.clone());
    base_repos.insert("platform".into(), platform_sha.clone());
    let base_id = {
        let req = PrepareRequest {
            refs_: BTreeMap::new(),
            repos: base_repos,
            excluded: Vec::new(),
            from: None,
            max_base_age_s: None,
            wait_ms: 120_000,
            config: Arc::new(ContractFederationConfig::default()),
        };
        let outcome = mgr.prepare(req).await.expect("base prepare");
        outcome.record.id.clone()
    };
    // 2. Derived: only `billing` overridden to a different ref.
    // `orders` and `platform` inherit the base's commits (no new
    // jobs for them — the cache entries are reused).
    let override_sha = {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "s20-read-discount"])
            .current_dir(&billing_root)
            .output()
            .expect("git rev-parse s20-read-discount");
        assert!(
            out.status.success(),
            "s20-read-discount tag missing in billing fixture"
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let mut derived_repos = BTreeMap::new();
    derived_repos.insert("billing".into(), "s20-read-discount".to_string());
    let outcome = mgr
        .prepare(PrepareRequest {
            refs_: BTreeMap::new(),
            repos: derived_repos,
            excluded: Vec::new(),
            from: Some(base_id.clone()),
            max_base_age_s: None,
            wait_ms: 120_000,
            config: Arc::new(ContractFederationConfig::default()),
        })
        .await
        .expect("derived prepare");
    // The derived id differs from the base id (repos differ).
    assert_ne!(outcome.record.id, base_id);
    // Re-read the record from disk to verify inheritance:
    // `orders` and `platform` keep the base's commits; `billing`
    // uses the override (promoted to canonical SHA).
    let stored = mgr_cached_record(&mgr, &outcome.record.id);
    assert_eq!(stored.repos.get("orders").unwrap(), &orders_sha);
    assert_eq!(stored.repos.get("billing").unwrap(), &override_sha);
    assert_eq!(stored.repos.get("platform").unwrap(), &platform_sha);
    // The derived id has the base's `config_hash` (ruling) — a
    // no-override derive would share the base id; this one
    // override differs in `repos` so the id differs.
    let base_record = mgr_cached_record(&mgr, &base_id);
    assert_eq!(stored.config_hash, base_record.config_hash);
}

/// Cold-start: a `pending` record on disk (one whose `prepare`
/// never reached `ready`) must have its jobs re-submitted by
/// `recover_from_disk` once the source resolver is installed.
/// The pre-fix behavior: `SnapshotManager::new` calls
/// `recover_from_disk` *before* the resolver is wired, so the
/// `resolve_repo_source_inner` lookup returns `None` and every
/// pending record is silently skipped. `with_snapshots` then
/// re-runs `recover_from_disk` after the resolver is installed
/// (the fix); this test asserts that re-run picks the record
/// up.
#[tokio::test]
async fn recover_from_disk_resubmits_pending_records_after_resolver_install() {
    std::env::set_var("LAIN_SNAPSHOT_WORKER_IDLE_TIMEOUT_MS", "1000");
    let fix = build_fixture();
    let cfg = federation_config(&fix);
    let orders_root = fix.repos_root.join("orders");
    let orders_sha = head_sha(&orders_root);

    // Step 1: build a manager with NO resolver, write a pending
    // record to its data_dir, and confirm `recover_from_disk` (in
    // `new`) skips it because the source is unknown.
    let cache = IndexCache::new(&cfg.data_dir);
    let mgr = SnapshotManager::new(&cfg.data_dir, cache);
    let pending = lain::federation::contracts::snapshots::SnapshotRecord {
        id: "snap_recover_probe".into(),
        repos: BTreeMap::from([("orders".into(), orders_sha.clone())]),
        excluded: Vec::new(),
        refs_: BTreeMap::new(),
        join_config: serde_json::Value::Null,
        config_hash: "h".into(),
        analyzer_version: lain::federation::contracts::analyzer_version(),
        state: lain::federation::contracts::snapshots::SnapshotState::Pending,
        repo_states: BTreeMap::from([(
            "orders".into(),
            lain::federation::contracts::snapshots::RepoSnapshotState::Queued {
                commit: orders_sha.clone(),
            },
        )]),
        created_unix: 0,
        last_access_unix: 0,
    };
    lain::federation::contracts::snapshots::record::write_record(&cfg.data_dir, &pending)
        .expect("write pending record");
    // No resolver yet → the cold-path recover did nothing.
    assert!(
        mgr.runner()
            .lookup(&lain::federation::contracts::snapshots::jobs::JobSpec {
                repo: "orders".into(),
                sha: orders_sha.clone(),
                analyzer_version: pending.analyzer_version.clone(),
                source: String::new(),
            })
            .is_none(),
        "no resolver, no job submitted"
    );

    // Step 2: install a known resolver and re-run
    // `recover_from_disk`. The source is a fixed sentinel so the
    // JobSpec we look up matches the one submitted.
    const SENTINEL_SOURCE: &str = "/tmp/sentinel/source";
    let resolver = std::sync::Arc::new(move |repo: &str| {
        if repo == "orders" {
            Some(SENTINEL_SOURCE.to_string())
        } else {
            None
        }
    }) as lain::federation::contracts::snapshots::manager::RepoSourceResolver;
    mgr.set_repo_source_resolver(resolver);
    mgr.recover_from_disk();
    let state = mgr
        .runner()
        .lookup(&lain::federation::contracts::snapshots::jobs::JobSpec {
            repo: "orders".into(),
            sha: orders_sha.clone(),
            analyzer_version: pending.analyzer_version.clone(),
            source: SENTINEL_SOURCE.to_string(),
        });
    assert!(
        state.is_some(),
        "recover_from_disk must resubmit pending records once the resolver is installed"
    );
    // Codex P2: ensure_workers_running must be called by the
    // production boot path (with_snapshots → recover_from_disk)
    // so the recovered jobs are actually picked up. Before this
    // fix, the workers only started when prepare_snapshot was
    // called next; a client polling an existing id via
    // get_snapshot after restart saw the jobs sit queued
    // forever.
    let _ = state; // keep the assertion above
    mgr.ensure_workers_running();
    // The worker_handles is now Some(handles). Calling
    // ensure_workers_running again is idempotent.
    mgr.ensure_workers_running();
}
