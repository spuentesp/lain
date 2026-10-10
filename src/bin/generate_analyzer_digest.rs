//! Generate `tests/fixtures/contracts/analyzer_digest.txt` from
//! the §15.1 fixture (PR 10, §8.3).
//!
//! Hermetic, no network: runs the fixture script into a tempdir,
//! snapshot-indexes the `orders` repo at its `base` commit, and
//! writes the parsed digest fixture. The script is meant to be
//! invoked once when the analyzer output changes (after a bump
//! of `CONTRACT_ANALYZER_REV`):
//!
//! ```text
//! cargo run --bin generate_analyzer_digest
//! ```
//!
//! Writes to `tests/fixtures/contracts/analyzer_digest.txt`
//! relative to the crate root. Re-running under the same
//! `CONTRACT_ANALYZER_REV` is a no-op overwrite; the test
//! `committed_digest_matches_fresh_recomputation` is the gate.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use lain::federation::contracts::digest::render_digest_fixture;
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::{RepoSource, WorkspaceDirSource};
use lain::git::{AnyGitSensor, GitSensorMode};
use lain::graph::GraphDatabase;
use lain::schema::RepoNamespace;
use lain::server::ingest::ingestion::{index_one_repo, IndexMode, IndexRequest};

/// `bash` for the fixture script: on Windows a bare `bash` is the WSL launcher,
/// so prefer Git for Windows' own; `LAIN_TEST_BASH` overrides.
fn git_bash() -> std::ffi::OsString {
    if let Some(p) = std::env::var_os("LAIN_TEST_BASH") {
        return p;
    }
    if cfg!(windows) {
        for cand in [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
        ] {
            if std::path::Path::new(cand).exists() {
                return cand.into();
            }
        }
    }
    "bash".into()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_script = manifest_dir.join("scripts").join("contracts-fixture.sh");
    let output_path = manifest_dir
        .join("tests")
        .join("fixtures")
        .join("contracts")
        .join("analyzer_digest.txt");

    // Build the fixture into a fresh tempdir so a future fixture
    // edit cannot make the committed digest stale by accident.
    let fixture_root = make_tempdir("lain-fixture")?;
    let status = Command::new(git_bash())
        .arg(&fixture_script)
        .arg(&fixture_root)
        .status()
        .map_err(|e| format!("spawn contracts-fixture.sh: {e}"))?;
    assert!(
        status.success(),
        "contracts-fixture.sh failed: exit {status:?}"
    );
    let orders_path = fixture_root.join("orders");

    let data_dir = make_tempdir("lain-snapshot")?;
    let graph_path = data_dir.join("graph.bin");
    let db = GraphDatabase::new(&graph_path)?;

    let source: Arc<dyn RepoSource> = Arc::new(
        WorkspaceDirSource::new(RepoId::new("orders")?, orders_path.clone())
            .map_err(|e| format!("WorkspaceDirSource: {e}"))?,
    );
    let local_path = source.local_path().to_path_buf();
    let git = AnyGitSensor::new(&local_path, GitSensorMode::InProcess)
        .map_err(|e| format!("AnyGitSensor: {e}"))?;

    let namespace = RepoNamespace::for_test();
    let cancel = tokio_util::sync::CancellationToken::new();
    let request = IndexRequest {
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
        mode: IndexMode::Snapshot,
    };
    index_one_repo(request)
        .await
        .map_err(|e| format!("index_one_repo: {e}"))?;

    let text = render_digest_fixture(&db);
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&output_path, &text)?;
    eprintln!("wrote {} ({} bytes)", output_path.display(), text.len());
    Ok(())
}

/// Make a unique tempdir without pulling in `tempfile` (which is
/// only a dev-dependency). The returned path is removed on best-
/// effort at process exit; the binary is short-lived so a leak is
/// harmless.
fn make_tempdir(tag: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let path = std::env::temp_dir().join(format!("{tag}-{pid}-{nanos}"));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
