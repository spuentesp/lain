//! Regression test for the federation overlay's tree-sitter fallback.
//!
//! Inventory note (parked-bug #11):
//! `scan_produces_symbol_nodes_without_lsp` covers the single-file
//! scanner's tree-sitter fallback (`src/server/ingest/scan.rs:387`)
//! but the federation's `RepoIndex::sync_overlay` →
//! `process_overlay_change` overlay-population path was uncovered.
//! `process_overlay_change` was previously broken (fixed in commit
//! `1bdd2f1`'s neighbourhood) and this test pins the contract that
//! the federation's volatile overlay actually receives tree-sitter
//! symbols when LSP is unavailable.
//!
//! Strategy: skip when `rust-analyzer` is on `$PATH` (the only
//! deterministic way to force the no-LSP arm from an integration
//! test — `mark_unavailable` is `#[cfg(test)]` and not reachable
//! from external crates). Local dev boxes will skip; CI runners
//! without rust-analyzer will run the check.

use std::path::Path;
use std::sync::Arc;

use lain::federation::repo_id::RepoId;
use lain::server::federation::repo_index::RepoIndex;
use lain::server::federation::repo_source::WorkspaceDirSource;

/// Initialize a temp directory as a git repo with one commit so
/// `RepoIndex::new` and `GitSensor::new` accept it.
///
/// Copied verbatim from `tests/federation_integration.rs:20-46`
/// (and `tests/watcher_freshness.rs:12-28`, byte-identical). Kept
/// local to this test file so the regression guard has zero
/// cross-file dependencies.
fn init_temp_git_repo(path: &Path) {
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "--initial-branch=main"]);
    git(&["config", "user.email", "no-lsp@test"]);
    git(&["config", "user.name", "no-lsp-test"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);
}

/// The headline regression. Skips when rust-analyzer is on PATH —
/// see module docs for why.
#[tokio::test]
async fn sync_overlay_populates_volatile_overlay_from_tree_sitter_when_lsp_unavailable() {
    // Skip if rust-analyzer is present: the federation's `process_overlay_change`
    // only walks the `Err` arm (and thus increments `lsp_failures`) when LSP is
    // unavailable. CI without rust-analyzer exercises the regression check.
    // Dev boxes with rust-analyzer skip with a clear log line.
    if which::which("rust-analyzer").is_ok() {
        eprintln!(
            "[skip] rust-analyzer on PATH; cannot exercise no-LSP overlay fallback. \
             Run on a CI runner without rust-analyzer to verify."
        );
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let repo_dir = tmp.path().to_path_buf();
    let src_dir = repo_dir.join("src");
    std::fs::create_dir_all(&src_dir).expect("mkdir src");

    // Seed: one committed file so `GitSensor::new` accepts the repo.
    std::fs::write(src_dir.join("lib.rs"), "pub fn existing() {}\n").expect("write lib");
    init_temp_git_repo(&repo_dir);

    // The file under test: added AFTER the initial commit so
    // `get_uncommitted_changes` returns it as an untracked change
    // and `process_overlay_change` exercises the tree-sitter
    // fallback against a real symbol body. Two symbols (function +
    // struct + a function-with-return-type) so the assertion
    // doesn't depend on any one specific tree-sitter match.
    std::fs::write(
        src_dir.join("new_module.rs"),
        "pub fn new_no_lsp_symbol() -> u32 { 7 }\n\
         pub struct NewNoLspStruct { pub x: i32 }\n",
    )
    .expect("write new_module");

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("mkdir data");
    let source = WorkspaceDirSource::new(RepoId::new("no_lsp").expect("repo id"), repo_dir)
        .expect("workspace dir source");
    let ri = Arc::new(RepoIndex::new(Box::new(source), &data_dir).expect("repo index"));

    // The call under test: federation's working-tree overlay refresh.
    // Drives `process_overlay_change` per uncommitted change →
    // `ensure_server` errors on missing rust-analyzer → falls
    // back to `treesitter::extract_definitions` → `overlay.insert_node`.
    ri.sync_overlay().await.expect("sync_overlay should succeed");

    // Assert (1): the federation actually walked the LSP-fail arm.
    // `sync_state` reads this counter into `RefreshOutcome`; an
    // assertion here proves we're not silently passing on an
    // empty-tree case or a stub LSP. We assert >= 1 rather than
    // exactly 1 because `get_uncommitted_changes` may surface
    // the same untracked file twice (once via the workdir-index
    // diff, once via `statuses().is_wt_new()`), so the
    // per-process-overlay-change counter is whatever the union
    // of those iterations produces.
    let failures = ri.last_overlay_lsp_failures();
    assert!(
        failures >= 1,
        "sync_overlay should have recorded >=1 LSP failure for \
         new_module.rs when rust-analyzer is unavailable; got {failures}"
    );

    // Assert (2): the volatile overlay is non-empty — i.e. the
    // tree-sitter fallback actually wrote symbols into the
    // overlay that `find_anchors` and `get_health`'s "Volatile
    // Nodes (Overlay)" count both read. An empty overlay here is
    // the exact failure mode the inventory note described: the
    // federation overlay stays empty whenever LSP can't deliver
    // symbols. We don't assert on specific symbol names because
    // the duplicate-call quirk of `process_overlay_change`
    // (same untracked file surfaces twice from
    // `get_uncommitted_changes`) makes name-level assertions
    // brittle; the regression we're guarding is "overlay is
    // empty when LSP unavailable", not "every specific symbol
    // is in the overlay".
    let overlay = ri.server_overlay();
    let nodes = overlay.get_all_nodes();
    assert!(
        !nodes.is_empty(),
        "sync_overlay should populate the volatile overlay from \
         tree-sitter when LSP is unavailable; overlay is empty"
    );

    // Assert (3): at least one node has line ranges populated so
    // `get_node_at_location` and `find_anchors` can resolve the
    // symbol at a source line. Same invariant
    // `scan_produces_symbol_nodes_without_lsp` pins at
    // `src/server/ingest/scan.rs:425-433`.
    assert!(
        nodes
            .iter()
            .any(|n| n.line_start.is_some() && n.line_end.is_some()),
        "at least one tree-sitter-derived node must carry \
         line_start/line_end so the overlay is queryable; got {} node(s)",
        nodes.len()
    );

    // `notify` Drop chain can panic on 6.x; per the
    // `watcher_freshness` pattern, forget both the index and the
    // tempdir to keep the test cleanup silent. See
    // `tests/federation_integration.rs:184-192`.
    std::mem::forget(ri);
    std::mem::forget(tmp);
}
