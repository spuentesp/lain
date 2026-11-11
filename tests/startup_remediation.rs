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
//! | `LAIN_STARTUP_DIGEST_WRITE`     | `1` = this run may (re)write the digest   |
//!
//! # The digest check fails closed
//!
//! The digest is a function of the **input tree**, not of the analyzer
//! alone: adding a function to the checkout changes the indexed node set
//! and therefore the digest. A check that quietly rewrote its own
//! baseline would pass on any tree and prove nothing, so the baseline is
//! only ever written by an explicit
//! `LAIN_STARTUP_DIGEST_WRITE=1` run. Without that variable a missing
//! baseline is a **test failure**.
//!
//! # Comparing analyzers across a code change
//!
//! To compare the pre-change analyzer with the post-change one, both must
//! see the *same input*. The input is a **worktree pinned to the
//! pre-change commit**; the operator creates it and the harness never
//! creates or moves a worktree itself:
//!
//! ```bash
//! # one-time setup: a detached worktree at the pre-change commit
//! git worktree add --detach /tmp/lain-startup-input-ea91ef8c ea91ef8c
//!
//! # (1) seed the baseline — MUST run with the PRE-CHANGE analyzer, i.e.
//! #     before any commit that touches src/. This is the only run that
//! #     may write the baseline, and it must be an explicit write run.
//! LAIN_STARTUP_DIGEST_WRITE=1 \
//! LAIN_STARTUP_REPO_ROOT=/tmp/lain-startup-input-ea91ef8c \
//! LAIN_STARTUP_DIGEST_FILE=/tmp/lain-startup-baseline.digest \
//!   cargo test --release --test startup_remediation \
//!     real_checkout_digest_matches_baseline -- --ignored --nocapture
//!
//! # (2) later, with the CHANGED analyzer, against the SAME pinned input
//! #     worktree. Never write here: the digest must equal the baseline
//! #     the pre-change analyzer produced.
//! LAIN_STARTUP_REPO_ROOT=/tmp/lain-startup-input-ea91ef8c \
//! LAIN_STARTUP_DIGEST_FILE=/tmp/lain-startup-baseline.digest \
//!   cargo test --release --test startup_remediation \
//!     real_checkout_digest_matches_baseline -- --ignored --nocapture
//! ```
//!
//! Pointing `LAIN_STARTUP_REPO_ROOT` at the moving checkout is *not* an
//! equivalence check: every commit that adds or edits a tracked file
//! changes the indexed node set and therefore the digest, which says
//! nothing about the analyzer.

mod common;

use common::parse_budget_ms;
use lain::federation::contracts::digest::canonical_digest_hex;
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::{RepoSource, WorkspaceDirSource};
use lain::git::{AnyGitSensor, GitSensorMode};
use lain::graph::GraphDatabase;
use lain::schema::RepoNamespace;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[test]
fn configured_budget_rejects_zero() {
    assert!(std::panic::catch_unwind(|| parse_budget_ms(Some("0"))).is_err());
}

/// True only for the exact opt-in value `"1"`. Anything else — including
/// `"true"`, `"0"` and the empty string — leaves the baseline read-only.
fn digest_write_requested(value: Option<&str>) -> bool {
    value == Some("1")
}

/// Compare `fresh` against the recorded baseline at `baseline`.
///
/// `write` is the operator's explicit opt-in
/// (`LAIN_STARTUP_DIGEST_WRITE=1`). Only such a run may create or
/// overwrite the baseline; every other run compares, and a missing
/// baseline is an error rather than a silent self-seed.
fn check_digest_against_baseline(baseline: &Path, fresh: &str, write: bool) -> Result<(), String> {
    if write {
        std::fs::write(baseline, format!("{fresh}\n")).map_err(|e| {
            format!(
                "could not write digest baseline {}: {e}",
                baseline.display()
            )
        })?;
        return Ok(());
    }
    let recorded = std::fs::read_to_string(baseline).map_err(|e| {
        format!(
            "digest baseline {} could not be read ({e}). The digest check fails closed: \
             it never creates its own baseline. Create it deliberately with \
             LAIN_STARTUP_DIGEST_WRITE=1.",
            baseline.display()
        )
    })?;
    let recorded = recorded.trim();
    if recorded == fresh {
        Ok(())
    } else {
        Err(format!(
            "canonical digest mismatch.\n  baseline: {recorded}\n  fresh:    {fresh}\n\
             The analyzer produced a different graph for the same input tree. If the \
             input tree changed, re-pin it and regenerate the baseline deliberately \
             with LAIN_STARTUP_DIGEST_WRITE=1."
        ))
    }
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

/// A missing baseline must be a failure, never a silent self-seed: a
/// self-seeding check passes on any tree and proves nothing.
#[test]
fn digest_check_fails_closed_when_the_baseline_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let baseline = dir.path().join("absent.digest");
    let err = check_digest_against_baseline(&baseline, "deadbeef", false)
        .expect_err("a missing baseline must be a failure");
    assert!(
        err.contains("LAIN_STARTUP_DIGEST_WRITE=1"),
        "the error must name the opt-in variable, got: {err}"
    );
    assert!(
        !baseline.exists(),
        "a read-only run must not create the baseline"
    );
}

/// Only an explicit write run may create or overwrite the baseline.
#[test]
fn digest_check_writes_only_when_explicitly_requested() {
    let dir = tempfile::tempdir().unwrap();
    let baseline = dir.path().join("baseline.digest");

    check_digest_against_baseline(&baseline, "deadbeef", true).expect("explicit write");
    assert_eq!(
        std::fs::read_to_string(&baseline).unwrap().trim(),
        "deadbeef"
    );

    // A write run may also overwrite an existing baseline.
    check_digest_against_baseline(&baseline, "c0ffee", true).expect("explicit overwrite");
    assert_eq!(std::fs::read_to_string(&baseline).unwrap().trim(), "c0ffee");
}

/// The comparison path: same digest passes, different digest fails.
#[test]
fn digest_check_compares_against_an_existing_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let baseline = dir.path().join("baseline.digest");
    std::fs::write(&baseline, "deadbeef\n").unwrap();

    check_digest_against_baseline(&baseline, "deadbeef", false).expect("matching digest");
    let err = check_digest_against_baseline(&baseline, "c0ffee", false)
        .expect_err("a different digest must fail");
    assert!(err.contains("mismatch"), "got: {err}");
}

/// The write opt-in is exact: nothing but `"1"` enables it.
#[test]
fn digest_write_is_opt_in_and_explicit() {
    assert!(!digest_write_requested(None));
    assert!(!digest_write_requested(Some("")));
    assert!(!digest_write_requested(Some("0")));
    assert!(!digest_write_requested(Some("true")));
    assert!(!digest_write_requested(Some("yes")));
    assert!(digest_write_requested(Some("1")));
}

#[tokio::test]
#[ignore]
async fn real_checkout_digest_matches_baseline() {
    let root = PathBuf::from(std::env::var("LAIN_STARTUP_REPO_ROOT").unwrap());
    let baseline = PathBuf::from(std::env::var("LAIN_STARTUP_DIGEST_FILE").unwrap());
    let (_tmp, db) = static_index(&root).await;
    let fresh = canonical_digest_hex(&db);
    let write = digest_write_requested(std::env::var("LAIN_STARTUP_DIGEST_WRITE").ok().as_deref());
    if let Err(message) = check_digest_against_baseline(&baseline, &fresh, write) {
        panic!("{message}");
    }
}
