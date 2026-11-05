//! Real-checkout startup baseline harness (startup remediation, task 1).
//!
//! Two kinds of evidence live here:
//!
//! 1. **Canonical-graph equivalence.** `real_checkout_digest_matches_baseline`
//!    indexes a real checkout with the live static path (no LSP) and
//!    compares the canonical analyzer digest against a stored baseline
//!    file. Later optimization tasks must not change the graph the
//!    analyzer produces; this is the tripwire.
//!
//! 2. **Startup budget parsing.** `configured_budget_rejects_zero`
//!    pins the rule that a configured budget must be strictly positive —
//!    a zero budget would otherwise turn every startup assertion into a
//!    coin flip.
//!
//! Both the digest baseline and the startup benchmark are driven by
//! environment variables so the same harness can be pointed at any
//! checkout:
//!
//! | variable                        | meaning                                   |
//! |---------------------------------|-------------------------------------------|
//! | `LAIN_STARTUP_REPO_ROOT`        | checkout to index / benchmark             |
//! | `LAIN_STARTUP_DIGEST_FILE`      | file holding the canonical digest        |
//! | `LAIN_STARTUP_BUDGET_MS`        | per-run static-readiness budget          |
//! | `LAIN_STARTUP_BASELINE_RSS_BYTES` | peak-RSS ceiling for later tasks        |

use lain::federation::contracts::digest::canonical_digest_hex;
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::{RepoSource, WorkspaceDirSource};
use lain::git::{AnyGitSensor, GitSensorMode};
use lain::graph::GraphDatabase;
use lain::schema::RepoNamespace;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[allow(dead_code)]
fn parse_budget_ms(value: Option<&str>) -> u64 {
    value
        .map(|value| {
            let budget = value.parse::<u64>().expect("numeric startup budget");
            assert!(budget > 0, "startup budget must be greater than zero");
            budget
        })
        .unwrap_or(60_000)
}

#[test]
fn configured_budget_rejects_zero() {
    assert!(std::panic::catch_unwind(|| parse_budget_ms(Some("0"))).is_err());
}

async fn static_index(root: &Path) -> (tempfile::TempDir, GraphDatabase) {
    let data_dir = tempfile::tempdir().unwrap();
    let db = GraphDatabase::new(&data_dir.path().join("graph.bin")).unwrap();
    let source: Arc<dyn RepoSource> = Arc::new(
        WorkspaceDirSource::new(RepoId::new("startup").unwrap(), root.to_path_buf()).unwrap(),
    );
    let local_path = source.local_path().to_path_buf();
    let git = AnyGitSensor::new(&local_path, GitSensorMode::InProcess).unwrap();
    let namespace = RepoNamespace::for_test();
    let cancel = tokio_util::sync::CancellationToken::new();
    lain::server::ingest::ingestion::index_one_repo(
        lain::server::ingest::ingestion::IndexRequest {
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
            mode: lain::server::ingest::ingestion::IndexMode::Live,
        },
    )
    .await
    .unwrap();
    (data_dir, db)
}

#[tokio::test]
#[ignore]
async fn real_checkout_digest_matches_baseline() {
    let root = PathBuf::from(std::env::var("LAIN_STARTUP_REPO_ROOT").unwrap());
    let baseline = PathBuf::from(std::env::var("LAIN_STARTUP_DIGEST_FILE").unwrap());
    let (_tmp, db) = static_index(&root).await;
    let fresh = canonical_digest_hex(&db);
    if baseline.exists() {
        assert_eq!(std::fs::read_to_string(&baseline).unwrap().trim(), fresh);
    } else {
        std::fs::write(&baseline, format!("{fresh}\n")).unwrap();
    }
}
