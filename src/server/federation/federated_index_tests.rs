//! Contract tests for `FederatedIndex` (Task 10).
//!
//! These exercise the orchestrator surface defined in the brief:
//! - `new`, `add_repo`, `list_repos`, `global_id`
//! - `resolve_symbol` (single match, no match, ambiguous)
//!
//! The `project_repo` / cross-repo matching path is exercised end-to-end via
//! `add_repo` + `backend().upsert_node_global(...)` plus `resolve_symbol` —
//! the matching path is its own concern in `matching_tests.rs`.
use crate::federation::federated_index::FederatedIndex;
use crate::federation::graph_backend::{GraphBackend, PetgraphBackend};
use crate::federation::repo_id::RepoId;
use crate::federation::repo_source::{RepoSource, WorkspaceDirSource};
use crate::schema::NodeType;
use crate::server::overlay::VolatileOverlay;
use std::sync::Arc;

fn petgraph_backend(tmp: &tempfile::TempDir) -> Arc<dyn GraphBackend> {
    Arc::new(PetgraphBackend::new(tmp.path()).unwrap())
}

#[tokio::test]
async fn add_repo_registers_and_lists_it() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(petgraph_backend(&tmp));
    // `RepoIndex::new` instantiates a `GitSensor` against the source's local
    // path, so the path must exist *and* be a real git repo. Initialize a
    // throwaway repo in a fresh tempdir; the test's behavior (add a repo,
    // list it, verify id) is unchanged.
    let src_dir = tempfile::tempdir().unwrap();
    git2::Repository::init(src_dir.path()).unwrap();
    let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("repo-a").unwrap(), src_dir.path().to_path_buf())
            .unwrap(),
    );
    fed.add_repo(src, tmp.path()).await.unwrap();
    let listed = fed.list_repos();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0.as_str(), "repo-a");
}

#[tokio::test]
async fn global_id_format() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(petgraph_backend(&tmp));
    let id = fed.global_id(
        &RepoId::new("repo-a").unwrap(),
        NodeType::Function,
        "src/lib.rs",
        "f",
    );
    assert_eq!(id.as_str(), "repo-a:Function:src/lib.rs:f");
}

#[test]
fn resolve_symbol_unique_match_returns_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(petgraph_backend(&tmp));
    let backend = fed.backend();
    backend
        .upsert_node_global(
            "repo-a:Function:src/lib.rs:only_one",
            NodeType::Function,
            "src/lib.rs",
            "only_one",
        )
        .unwrap();
    let resolved = fed.resolve_symbol("only_one").unwrap();
    assert_eq!(resolved.as_str(), "repo-a");
}

#[test]
fn resolve_symbol_no_match_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(petgraph_backend(&tmp));
    assert!(matches!(
        fed.resolve_symbol("nope"),
        Err(crate::error::LainError::NotFound(_))
    ));
}

#[test]
fn resolve_symbol_multiple_matches_returns_ambiguous() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(petgraph_backend(&tmp));
    let backend = fed.backend();
    backend
        .upsert_node_global(
            "repo-a:Function:src/lib.rs:shared",
            NodeType::Function,
            "src/lib.rs",
            "shared",
        )
        .unwrap();
    backend
        .upsert_node_global(
            "repo-b:Function:src/lib.rs:shared",
            NodeType::Function,
            "src/lib.rs",
            "shared",
        )
        .unwrap();
    let err = fed.resolve_symbol("shared").unwrap_err();
    assert!(matches!(err, crate::error::LainError::AmbiguousSymbol(_)));
}

/// A symbol defined more than once inside a *single* repo is not
/// ambiguous. The fast-path symbol index pushed one entry per
/// definition, so `resolve_symbol` returned
/// `AmbiguousSymbol(["lain", "lain"])` — asking the caller to
/// disambiguate between one repo and itself, through a `repo_id`
/// parameter the tool schema does not expose.
#[test]
fn distinct_repos_collapses_repeated_definitions_in_one_repo() {
    use crate::federation::federated_index::distinct_repos;
    let lain = RepoId::new("lain").unwrap();
    // Three definitions of `parse` in one repo.
    let entries = vec![lain.clone(), lain.clone(), lain.clone()];
    assert_eq!(distinct_repos(&entries), vec![lain]);
}

fn git_repo_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git2::Repository::init(dir.path()).unwrap();
    dir
}

async fn add_repo_with_id(
    fed: &FederatedIndex,
    data_dir: &std::path::Path,
    id: &str,
) -> Arc<crate::federation::repo_index::RepoIndex> {
    let src_dir = git_repo_dir();
    // Leak the tempdir so its path outlives this call — the repo's
    // `WorkspaceDirSource` only needs the path to exist, not the node's
    // content, so nothing in this test reads from it after `add_repo`.
    let src_path = src_dir.keep();
    let src: Box<dyn crate::federation::repo_source::RepoSource> =
        Box::new(WorkspaceDirSource::new(RepoId::new(id).unwrap(), src_path).unwrap());
    fed.add_repo(src, data_dir).await.unwrap();
    fed.get_repo(&RepoId::new(id).unwrap()).unwrap()
}

/// `project_repo`'s cross-repo matching used to upsert a bare 4-field
/// placeholder (kind/path/name only) for the matched peer node,
/// discarding whatever richer data — signature, line numbers,
/// docstring, embedding — that node already had in the backend.
/// `GraphDatabase::upsert_node`'s "only overwrite if hydrated" guard
/// does not protect against this: `GraphNode::new` defaults
/// `is_hydrated: true`, so a bare placeholder built via `::new()` is
/// indistinguishable from a fully-detailed node to that guard, and
/// the overwrite always wins regardless of which one is richer.
///
/// `project_repo` calls are not sequenced relative to each other (the
/// federation loader can run them in parallel), so the "placeholder
/// first, real data later" ordering the original fix assumed is not
/// guaranteed. This pins the opposite, dangerous ordering: repo `b`
/// is projected (publishing its real, line-numbered node) *before*
/// repo `a`'s cross-repo match against it fires.
#[tokio::test]
async fn project_repo_cross_repo_match_does_not_strip_already_published_peer_data() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(petgraph_backend(&tmp));

    let repo_a = add_repo_with_id(&fed, tmp.path(), "a").await;
    let repo_b = add_repo_with_id(&fed, tmp.path(), "b").await;

    let mut node_a = crate::schema::GraphNode::new(
        NodeType::Function,
        "shared_helper".into(),
        "src/lib.rs".into(),
    );
    node_a.line_start = Some(10);
    node_a.line_end = Some(12);
    repo_a.db().insert_nodes_batch(&[node_a]).unwrap();

    let mut node_b = crate::schema::GraphNode::new(
        NodeType::Function,
        "shared_helper".into(),
        "src/lib.rs".into(),
    );
    node_b.line_start = Some(20);
    node_b.line_end = Some(22);
    repo_b.db().insert_nodes_batch(&[node_b]).unwrap();

    // `b` publishes its real, line-numbered node to the backend first.
    fed.project_repo(&RepoId::new("b").unwrap()).await.unwrap();
    let published = fed
        .backend()
        .get_node("b:Function:src/lib.rs:shared_helper")
        .unwrap();
    assert_eq!(
        published.and_then(|n| n.line_end),
        Some(22),
        "sanity: b's own projection publishes real line data"
    );

    // `a`'s cross-repo match against `b:...:shared_helper` fires here.
    // Before the fix, this call downgraded the node just published
    // above back to line_start/line_end == None.
    fed.project_repo(&RepoId::new("a").unwrap()).await.unwrap();

    let after = fed
        .backend()
        .get_node("b:Function:src/lib.rs:shared_helper")
        .unwrap();
    assert_eq!(
        after.and_then(|n| n.line_end),
        Some(22),
        "a's cross-repo match against b's node must not strip its already-published line data"
    );
}

#[test]
fn distinct_repos_keeps_genuine_cross_repo_ambiguity() {
    use crate::federation::federated_index::distinct_repos;
    let a = RepoId::new("repo-a").unwrap();
    let b = RepoId::new("repo-b").unwrap();
    // Same name in two repos really is ambiguous, and order is kept so
    // the reported candidate list is stable.
    let entries = vec![a.clone(), b.clone(), a.clone()];
    assert_eq!(distinct_repos(&entries), vec![a, b]);
}

#[test]
fn distinct_repos_on_empty_is_empty() {
    use crate::federation::federated_index::distinct_repos;
    assert!(distinct_repos(&[]).is_empty());
}

/// Two repos with identical `(type, path, name)` symbols must produce
/// distinct `GraphNode::id`s. Pre-fix, both ids were the same UUID v5
/// because `GraphNode::generate_id` only hashed the `(type, path, name,
/// line)` input — no repo identity. Two federation repos with a
/// function `foo` at `src/lib.rs:1` would mint the same id, and
/// `RepoIndex::sync_overlay`'s id-keyed cleanup would remove one
/// repo's overlay node when the other's `process_overlay_change`
/// fired. URGENT FIXES #2.
/// Real cross-repo collision regression. Two federation repos
/// with identical `(type, path, name)` symbols must produce
/// distinct `GraphNode::id`s when run through
/// `RepoIndex::process_overlay_change` — the production path that
/// mints overlay nodes. Pre-fix, both ids were the same UUID v5
/// because `GraphNode::generate_id` only hashed `(type, path, name,
/// line)` — no repo identity. Two repos with `pub fn
/// shared_symbol() {}` at `src/lib.rs:1` would mint the same id,
/// and the federation's shared `VolatileOverlay` would collapse
/// them into one entry; `RepoIndex::sync_overlay`'s id-keyed
/// cleanup would remove one repo's node when the other's
/// `sync_overlay` fired. URGENT FIXES #2.
///
/// Skipped when rust-analyzer is on PATH — the federation's
/// `process_overlay_change` only takes the `Err` arm (and
/// increments `lsp_failures`) when LSP is unavailable. With
/// rust-analyzer present the tree-sitter fallback is bypassed and
/// the test would race on a duplicate-call quirk of
/// `get_uncommitted_changes`. Same skip rationale as
/// `tests/federation_overlay_no_lsp.rs`.
#[tokio::test]
async fn cross_repo_overlay_inserts_distinct_ids_for_identical_symbols() {
    if which::which("rust-analyzer").is_ok() {
        eprintln!(
            "[skip] rust-analyzer on PATH; cross-repo overlay test              exercises the tree-sitter fallback which only fires              when LSP is unavailable. Run on a CI runner without              rust-analyzer to verify."
        );
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let repo_a_dir = tmp.path().join("a");
    let repo_b_dir = tmp.path().join("b");
    for d in [&repo_a_dir, &repo_b_dir] {
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("README.md"), "init\n").unwrap();
        git2::Repository::init(d).unwrap();
        // Configure git identity so the tree-sitter fallback can
        // run (the federation overlay path runs in CI without
        // rust-analyzer, so the fallback's the only source of
        // symbols).
    }

    // Add an uncommitted file with the SAME symbol name to both
    // repos. `get_uncommitted_changes` surfaces it for both repos
    // in their `sync_overlay` cycles.
    for d in [&repo_a_dir, &repo_b_dir] {
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(
            d.join("src/lib.rs"),
            "pub fn shared_symbol() -> u32 { 0 }\n",
        )
        .unwrap();
    }

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    // One shared overlay for both repos — that's the whole point
    // of the regression: identical symbols from two repos must
    // not collapse into one overlay entry.
    let shared_overlay = Arc::new(VolatileOverlay::new());
    let backend: Arc<dyn GraphBackend> =
        Arc::new(PetgraphBackend::new(&data_dir).unwrap());
    let fed = Arc::new(FederatedIndex::new(backend));
    fed.install_overlay(shared_overlay.clone());

    let src_a: Box<dyn RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("repo-a").unwrap(), repo_a_dir.clone()).unwrap(),
    );
    let src_b: Box<dyn RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("repo-b").unwrap(), repo_b_dir.clone()).unwrap(),
    );
    fed.add_repo(src_a, &data_dir).await.unwrap();
    fed.add_repo(src_b, &data_dir).await.unwrap();

    // Run each repo's working-tree overlay refresh. Each
    // `process_overlay_change` mints the symbol with its repo's
    // `id_namespace`. After both cycles the shared overlay must
    // hold two distinct nodes — one per repo.
    let repo_a = fed.get_repo(&RepoId::new("repo-a").unwrap()).unwrap();
    let repo_b = fed.get_repo(&RepoId::new("repo-b").unwrap()).unwrap();
    repo_a.sync_overlay().await.expect("sync_overlay repo-a");
    repo_b.sync_overlay().await.expect("sync_overlay repo-b");

    let overlay_nodes = shared_overlay.get_all_nodes();
    let shared: Vec<_> = overlay_nodes
        .iter()
        .filter(|n| n.name == "shared_symbol")
        .collect();
    assert_eq!(
        shared.len(),
        2,
        "two federation repos with identical 'shared_symbol' must          produce two distinct overlay entries; got {} total overlay          node(s) but only {} named shared_symbol",
        overlay_nodes.len(),
        shared.len()
    );
    assert_ne!(
        shared[0].id, shared[1].id,
        "the two repos must produce distinct overlay ids;          pre-PR-#14 these collided because `GraphNode::generate_id`          ignored repo identity. id_a={} id_b={}",
        shared[0].id, shared[1].id
    );

    // Cross-repo cleanup isolation. Remove repo-a and run repo-b's
    // `sync_overlay` again. Repo-a's overlay entries are NOT in
    // repo-b's `overlay_paths` bookkeeping, so repo-b's purge
    // (which only removes ids it itself inserted) leaves the
    // surviving repo's node alone.
    //
    // This is the test that demonstrates the bug the comment was
    // talking about: pre-fix, repo-b's `process_overlay_change`
    // inserted a symbol with the SAME id as repo-a's (because
    // namespace didn't differ); when repo-a was removed and
    // repo-b's next sync_overlay fired, repo-b's id-keyed cleanup
    // would have wiped its own entry too. Post-fix, repo-a and
    // repo-b's ids are distinct, so each repo's purge only touches
    // its own entries.
    let repo_a_id = RepoId::new("repo-a").unwrap();
    fed.remove_repo(&repo_a_id).unwrap();
    repo_b.sync_overlay().await.expect("sync_overlay repo-b after remove");

    let survivor: Vec<_> = shared_overlay
        .get_all_nodes()
        .into_iter()
        .filter(|n| n.name == "shared_symbol")
        .collect();
    assert_eq!(
        survivor.len(),
        1,
        "removing repo-a and re-syncing repo-b must leave repo-b's          'shared_symbol' node in the shared overlay; pre-fix the          same id would have been purged by repo-a's removal.          survived ids: {:?}",
        survivor.iter().map(|n| &n.id).collect::<Vec<_>>()
    );

    std::mem::forget(repo_a);
    std::mem::forget(repo_b);
    std::mem::forget(fed);
    std::mem::forget(shared_overlay);
    std::mem::forget(tmp);
}


// ── manifest persistence (issue #8) ────────────────────────────────
//
// `FederatedIndex::add_repo` and `remove_repo` must rewrite the
// configured manifest path so a runtime membership change survives a
// process restart. The tests below cover the contract.

/// `set_manifest_path(None)` makes the federation a non-persister:
/// `add_repo` is a no-op for the manifest. Useful for unit tests that
/// don't want a stray file under `target/tmp`.
#[tokio::test]
async fn add_repo_does_not_create_a_manifest_when_path_unset() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(petgraph_backend(&tmp));
    // Default state: no path wired, so even after add_repo no file lands.
    let src_dir = tempfile::tempdir().unwrap();
    git2::Repository::init(src_dir.path()).unwrap();
    let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("a").unwrap(), src_dir.path().to_path_buf()).unwrap(),
    );
    fed.add_repo(src, tmp.path()).await.unwrap();
    assert!(
        !tmp.path().join("federation_manifest.bin").exists(),
        "no manifest should be written when set_manifest_path was never called",
    );
}

/// `add_repo` writes a manifest whose `source_config` matches the
/// `SourceConfig` the source was constructed with — round-tripping
/// works end-to-end through bincode.
#[tokio::test]
async fn add_repo_persists_source_config() {
    use crate::federation::config::SourceConfig;
    use crate::federation::manifest::FederationManifest;
    let tmp = tempfile::tempdir().unwrap();
    let manifest_path = tmp.path().join("federation_manifest.bin");
    let fed = FederatedIndex::new(petgraph_backend(&tmp));
    fed.set_manifest_path(Some(manifest_path.clone()));

    let src_dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(src_dir.path()).unwrap();
    // Commit so HEAD resolves cleanly. `RepoSource::content_hash`
    // returns Ok(None) on an unborn HEAD, and a `None` value
    // silently drops the entry from the persisted manifest — so a
    // test fixture must commit at least once to verify persistence.
    let sig = git2::Signature::now("test", "test@lain").unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let _ = repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]);

    let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("a").unwrap(), src_dir.path().to_path_buf()).unwrap(),
    );
    fed.add_repo(src, tmp.path()).await.unwrap();

    assert!(manifest_path.exists(), "manifest must be written on add_repo");
    let loaded = FederationManifest::load_or_default(&manifest_path).unwrap();
    assert_eq!(loaded.repos.len(), 1);
    assert_eq!(loaded.repos[0].id.as_str(), "a");
    assert_eq!(loaded.repos[0].source_kind, "workspace_dir");
    let expected = SourceConfig::WorkspaceDir { path: src_dir.path().to_path_buf() };
    assert_eq!(loaded.repos[0].source_config, serde_yaml::to_value(&expected).unwrap());
}

/// `add_repo` populates `content_hash` with the HEAD hash of the
/// underlying git repo. The hash is non-empty and matches what
/// `git rev-parse HEAD` says.
#[tokio::test]
async fn add_repo_persists_content_hash() {
    use crate::federation::manifest::FederationManifest;
    let tmp = tempfile::tempdir().unwrap();
    let manifest_path = tmp.path().join("federation_manifest.bin");
    let fed = FederatedIndex::new(petgraph_backend(&tmp));
    fed.set_manifest_path(Some(manifest_path.clone()));

    let src_dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(src_dir.path()).unwrap();
    let sig = git2::Signature::now("test", "test@lain").unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let _ = repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]);

    let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("a").unwrap(), src_dir.path().to_path_buf()).unwrap(),
    );
    fed.add_repo(src, tmp.path()).await.unwrap();

    let loaded = FederationManifest::load_or_default(&manifest_path).unwrap();
    assert_eq!(loaded.repos.len(), 1);
    let hash = &loaded.repos[0].content_hash;
    assert!(!hash.is_empty(), "git repo HEAD must yield a non-empty hash");
    assert_eq!(hash.len(), 40, "SHA-1 hex is 40 chars, got {hash:?}");
}

/// `remove_repo` rewrites the manifest with the removed repo gone.
/// Without the hook, the manifest would still list the dead repo after
/// restart — a stale entry the loader would later try to project into
/// the federation.
#[tokio::test]
async fn remove_repo_persists_membership_change() {
    use crate::federation::manifest::FederationManifest;
    let tmp = tempfile::tempdir().unwrap();
    let manifest_path = tmp.path().join("federation_manifest.bin");
    let fed = FederatedIndex::new(petgraph_backend(&tmp));
    fed.set_manifest_path(Some(manifest_path.clone()));

    let src_dir = tempfile::tempdir().unwrap();
    git2::Repository::init(src_dir.path()).unwrap();
    let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("a").unwrap(), src_dir.path().to_path_buf()).unwrap(),
    );
    fed.add_repo(src, tmp.path()).await.unwrap();

    fed.remove_repo(&RepoId::new("a").unwrap()).unwrap();
    let loaded = FederationManifest::load_or_default(&manifest_path).unwrap();
    assert!(loaded.repos.is_empty(), "remove_repo must rewrite the manifest");
}

/// A `git commit` on the source repo changes HEAD; a second
/// `add_repo` (or any subsequent persist) records the new hash.
/// Without this, a stale manifest would silently claim a content
/// version the federation no longer matches.
#[tokio::test]
async fn content_hash_changes_after_git_commit() {
    use crate::federation::config::SourceConfig;
    use crate::federation::manifest::FederationManifest;
    let tmp = tempfile::tempdir().unwrap();
    let manifest_path = tmp.path().join("federation_manifest.bin");
    let fed = FederatedIndex::new(petgraph_backend(&tmp));
    fed.set_manifest_path(Some(manifest_path.clone()));

    let src_dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(src_dir.path()).unwrap();
    // First commit so HEAD is well-defined.
    {
        let sig = repo.signature().unwrap_or_else(|_| git2::Signature::now("test", "test@lain").unwrap());
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let _ = repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]);
    }
    let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("a").unwrap(), src_dir.path().to_path_buf()).unwrap(),
    );
    fed.add_repo(src, tmp.path()).await.unwrap();
    let first = FederationManifest::load_or_default(&manifest_path).unwrap().repos[0].content_hash.clone();

    // Second commit on the same repo — HEAD moves forward.
    {
        let sig = git2::Signature::now("test", "test@lain").unwrap();
        let mut index = repo.index().unwrap();
        let path = src_dir.path().join("new.txt");
        std::fs::write(&path, "second\n").unwrap();
        index.add_path(std::path::Path::new("new.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let _ = repo.commit(Some("HEAD"), &sig, &sig, "second", &tree, &[&parent]);
    }

    // Touch the manifest by re-adding with the same source. We
    // can't easily trigger a "real" indexer re-run, so we just
    // remove + re-add to exercise `persist_manifest` again.
    fed.remove_repo(&RepoId::new("a").unwrap()).unwrap();
    let src2: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("a").unwrap(), src_dir.path().to_path_buf()).unwrap(),
    );
    fed.add_repo(src2, tmp.path()).await.unwrap();

    let second = FederationManifest::load_or_default(&manifest_path).unwrap().repos[0].content_hash.clone();
    assert_ne!(first, second, "content_hash must change after a git commit");

    // Touch unrelated: the manifest also keeps `source_config`
    // round-trippable after the re-add.
    let expected = SourceConfig::WorkspaceDir { path: src_dir.path().to_path_buf() };
    let loaded = FederationManifest::load_or_default(&manifest_path).unwrap();
    assert_eq!(loaded.repos[0].source_config, serde_yaml::to_value(&expected).unwrap());
}
