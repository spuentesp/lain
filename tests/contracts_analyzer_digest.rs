//! PR 10 analyzer digest pinning (`§8.3`).
//!
//! Two tests live here:
//!
//! 1. **Determinism**: indexing the same fixture sha twice yields
//!    the same canonical blake3 digest. The fixture is built from
//!    `scripts/contracts-fixture.sh` (`§15.1`), which is hermetic
//!    and writes four real git repos into a tempdir. The test
//!    uses those repos directly (no federation boot, no LSP) so
//!    it runs in the standard `cargo test` cycle.
//!
//! 2. **Committed digest**: the digest of the fixture's `orders`
//!    repo at its `base` commit must match
//!    `tests/fixtures/contracts/analyzer_digest.txt`. A mismatch
//!    without a corresponding bump of `CONTRACT_ANALYZER_REV`
//!    fails with a regenerate command. The committed fixture
//!    stores both the analyzer revision and the digest so a
//!    regeneration run that bumps the counter is detectable on
//!    its own (the analyzer-rev field changes).
//!
//! The integration test deliberately runs against a tempdir
//! copy of the fixture, not the working tree, so it cannot leak
//! commits into the developer's checkout and a future fixture
//! edit cannot make this test silently pass by accident.

use lain::federation::contracts::digest::{
    canonical_digest_hex, render_digest_fixture, ParsedDigestFixture,
};
use lain::federation::contracts::{analyzer_version, CONTRACT_ANALYZER_REV};
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::{RepoSource, WorkspaceDirSource};
use lain::git::{AnyGitSensor, GitSensorMode};
use lain::graph::GraphDatabase;
use lain::schema::RepoNamespace;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

/// Locate the fixture script (`scripts/contracts-fixture.sh`)
/// from the crate root. `CARGO_MANIFEST_DIR` is set by cargo at
/// compile time to the package root.
fn fixture_script() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("contracts-fixture.sh")
}

/// Run the fixture script into a fresh tempdir, return the tempdir
/// handle (the caller owns it) and the resolved path to the four
/// repos written by the script.
fn build_fixture() -> (tempfile::TempDir, FixtureRepos) {
    let tmp = tempfile::tempdir().expect("fixture tempdir");
    let root = tmp.path().to_path_buf();
    // Through `bash`: Windows cannot execute a `.sh` file directly
    // ("%1 is not a valid Win32 application"); CI has Git Bash.
    let status = Command::new("bash")
        .arg(fixture_script())
        .arg(&root)
        .status()
        .expect("spawn contracts-fixture.sh");
    assert!(
        status.success(),
        "contracts-fixture.sh failed: exit {status:?}"
    );
    let repos = FixtureRepos {
        orders: root.join("orders"),
        billing: root.join("billing"),
        reports: root.join("reports"),
        platform: root.join("platform"),
        root,
    };
    (tmp, repos)
}

#[allow(dead_code)]
struct FixtureRepos {
    orders: std::path::PathBuf,
    billing: std::path::PathBuf,
    reports: std::path::PathBuf,
    platform: std::path::PathBuf,
    root: std::path::PathBuf,
}

/// Resolve a repo's `base` commit sha via the `git` CLI. The
/// fixture tags every repo's initial commit `base`, so this is a
/// stable lookup that does not depend on HEAD.
fn base_sha(repo: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "base"])
        .current_dir(repo)
        .output()
        .expect("git rev-parse base");
    assert!(
        out.status.success(),
        "git rev-parse base failed in {}",
        repo.display()
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Index one fixture repo at its `base` commit, in snapshot mode
/// (`§8.2`), and return the per-repo `GraphDatabase` the snapshot
/// produced. The function is hermetic: a fresh tempdir holds the
/// per-repo DB, no LSP is constructed, and the index pass uses
/// `force = true` so the commit-hash short-circuit does not
/// interfere. This is the same shape the snapshot job runner
/// (PR 11) uses per the brief.
async fn snapshot_index(repo: &Path, repo_id: &str) -> (tempfile::TempDir, GraphDatabase) {
    let _ = repo_id;
    let data_dir = tempfile::tempdir().expect("snapshot tempdir");
    let graph_path = data_dir.path().join("graph.bin");
    let db = GraphDatabase::new(&graph_path).expect("empty GraphDatabase");

    let source: Arc<dyn RepoSource> = Arc::new(
        WorkspaceDirSource::new(RepoId::new("orders").unwrap(), repo.to_path_buf())
            .expect("WorkspaceDirSource"),
    );
    let local_path = source.local_path().to_path_buf();
    let git = AnyGitSensor::new(&local_path, GitSensorMode::InProcess).expect("AnyGitSensor");

    let namespace = RepoNamespace::for_test();
    let cancel = tokio_util::sync::CancellationToken::new();

    // Snapshot mode: no LSP, no overlay, no resolver, force = true
    // (the brief's §8.2 verbatim).
    let request = lain::server::ingest::ingestion::IndexRequest {
        path: &local_path,
        graph: &db,
        lsp_pool: None,
        git: &git,
        overlay: None,
        resolver: None,
        source_repo: None,
        namespace: &namespace,
        force: true,
        cancel: &cancel,
        mode: lain::server::ingest::ingestion::IndexMode::Snapshot,
    };
    lain::server::ingest::ingestion::index_one_repo(request)
        .await
        .expect("snapshot index_one_repo");
    (data_dir, db)
}

/// Indexing the same fixture sha twice must produce the same
/// canonical digest. The fixture writes one `base` commit per
/// repo, so we run the snapshot indexer twice against the same
/// path and assert the digests match. The first run hits a cold
/// `GraphDatabase`; the second runs against a fresh one too
/// (resets every per-instance state). Different graphs, same
/// fixture, same digest.
#[tokio::test]
async fn digest_is_deterministic_across_two_runs() {
    let (_fixture_tmp, repos) = build_fixture();
    let sha = base_sha(&repos.orders);
    assert_eq!(sha.len(), 40, "base commit must be a 40-hex sha");

    // First run.
    let (_db1_tmp, db1) = snapshot_index(&repos.orders, "orders").await;
    let digest1 = canonical_digest_hex(&db1);

    // Second run on a fresh DB. Same fixture path, same sha,
    // fresh `GraphDatabase`.
    let (_db2_tmp, db2) = snapshot_index(&repos.orders, "orders").await;
    let digest2 = canonical_digest_hex(&db2);

    assert_eq!(
        digest1, digest2,
        "canonical digest must be deterministic across two runs"
    );

    // Sanity: an empty graph digests to something else.
    let tmp = tempfile::tempdir().unwrap();
    let empty = GraphDatabase::new(&tmp.path().join("graph.bin")).unwrap();
    let empty_digest = canonical_digest_hex(&empty);
    assert_ne!(
        digest1, empty_digest,
        "an indexed fixture must not digest the same as an empty graph"
    );
}

/// The committed fixture's analyzer digest must match a fresh
/// recomputation under the current `CONTRACT_ANALYZER_REV`. A
/// mismatch without a bump fails with a regenerate command. The
/// fixture file is `tests/fixtures/contracts/analyzer_digest.txt`.
#[tokio::test]
async fn committed_digest_matches_fresh_recomputation() {
    let (_fixture_tmp, repos) = build_fixture();
    let (_db_tmp, db) = snapshot_index(&repos.orders, "orders").await;
    let fresh = render_digest_fixture(&db);
    let fresh_parsed = ParsedDigestFixture::parse(&fresh).expect("fresh fixture parses");

    let committed_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("contracts")
        .join("analyzer_digest.txt");
    let committed_text = std::fs::read_to_string(&committed_path).unwrap_or_else(|e| {
        panic!(
            "could not read committed digest fixture at {}: {e}. \
             The fixture is generated by `scripts/contracts-fixture.sh <dir>` + \
             a snapshot-mode `index_one_repo` + `render_digest_fixture`; \
             it must be committed alongside this test.",
            committed_path.display()
        )
    });
    let committed = ParsedDigestFixture::parse(&committed_text).expect("committed fixture parses");

    if committed.analyzer_rev != CONTRACT_ANALYZER_REV {
        panic!(
            "analyzer_rev in committed fixture ({}) does not match \
             CONTRACT_ANALYZER_REV ({}). Bump the constant in \
             src/server/federation/contracts/mod.rs and regenerate the \
             fixture with:\n\
             \n\
             ANCHOR: regenerate_analyzer_digest\n\
             \n\
             Run `scripts/contracts-fixture.sh <tmp>` into a tempdir, \
             snapshot-index the orders repo, then write the parsed \
             output to {}.\n",
            committed.analyzer_rev,
            CONTRACT_ANALYZER_REV,
            committed_path.display()
        );
    }

    if committed.digest != fresh_parsed.digest {
        panic!(
            "analyzer digest mismatch.\n  committed: {}\n  fresh:     {}\n\n\
             The fixture changed (a sensor, the normalizer, or the joiner \
             emitted different output) but CONTRACT_ANALYZER_REV was not \
             bumped. Either revert the change, or:\n\
             \n\
             ANCHOR: regenerate_analyzer_digest\n\
             \n\
             1. Bump CONTRACT_ANALYZER_REV in src/server/federation/contracts/mod.rs\n\
             2. Run the snapshot indexer on the fixture (see {} as a template)\n\
             3. Write the new analyzer_rev + digest to {}.\n",
            committed.digest,
            fresh_parsed.digest,
            file!(),
            committed_path.display(),
        );
    }

    // Belt-and-braces: the analyzer_version string must include
    // the current CONTRACT_ANALYZER_REV so cache entries keyed on
    // it stay distinct from earlier revisions.
    let av = analyzer_version();
    assert!(
        av.ends_with(&format!("+c{CONTRACT_ANALYZER_REV}")),
        "analyzer_version() should end with `+c{{rev}}`; got {av:?}"
    );
}

/// Snapshot mode skips the LSP, overlay, cross-repo resolver,
/// co-change pass, and NLP enrichment (§8.2). We assert the
/// indexer ran in the snapshot path by checking that the
/// `IndexRequest::mode` field round-trips: the digest test above
/// already constructs an `IndexRequest { mode: Snapshot, … }`,
/// so this test simply re-asserts the enum's documented shape so
/// a future refactor cannot silently turn `Snapshot` into a
/// no-op that runs everything anyway.
#[test]
fn index_mode_snapshot_skip_flags_are_consistent() {
    use lain::server::ingest::ingestion::IndexMode;
    assert!(IndexMode::Snapshot.skips_lsp());
    assert!(IndexMode::Snapshot.forces_full_rescan());
    assert!(!IndexMode::Live.skips_lsp());
    assert!(!IndexMode::Live.forces_full_rescan());
    assert_ne!(IndexMode::Live, IndexMode::Snapshot);
}
