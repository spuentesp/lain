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
    let backend: Arc<dyn GraphBackend> = Arc::new(PetgraphBackend::new(&data_dir).unwrap());
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
    repo_b
        .sync_overlay()
        .await
        .expect("sync_overlay repo-b after remove");

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

    assert!(
        manifest_path.exists(),
        "manifest must be written on add_repo"
    );
    let loaded = FederationManifest::load_or_default(&manifest_path).unwrap();
    assert_eq!(loaded.repos.len(), 1);
    assert_eq!(loaded.repos[0].id.as_str(), "a");
    assert_eq!(loaded.repos[0].source_kind, "workspace_dir");
    let expected = SourceConfig::WorkspaceDir {
        path: src_dir.path().to_path_buf(),
    };
    assert_eq!(
        loaded.repos[0].source_config,
        serde_yaml::to_value(&expected).unwrap()
    );
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
    assert!(
        !hash.is_empty(),
        "git repo HEAD must yield a non-empty hash"
    );
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
    assert!(
        loaded.repos.is_empty(),
        "remove_repo must rewrite the manifest"
    );
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
        let sig = repo
            .signature()
            .unwrap_or_else(|_| git2::Signature::now("test", "test@lain").unwrap());
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let _ = repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]);
    }
    let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("a").unwrap(), src_dir.path().to_path_buf()).unwrap(),
    );
    fed.add_repo(src, tmp.path()).await.unwrap();
    let first = FederationManifest::load_or_default(&manifest_path)
        .unwrap()
        .repos[0]
        .content_hash
        .clone();

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

    let second = FederationManifest::load_or_default(&manifest_path)
        .unwrap()
        .repos[0]
        .content_hash
        .clone();
    assert_ne!(first, second, "content_hash must change after a git commit");

    // Touch unrelated: the manifest also keeps `source_config`
    // round-trippable after the re-add.
    let expected = SourceConfig::WorkspaceDir {
        path: src_dir.path().to_path_buf(),
    };
    let loaded = FederationManifest::load_or_default(&manifest_path).unwrap();
    assert_eq!(
        loaded.repos[0].source_config,
        serde_yaml::to_value(&expected).unwrap()
    );
}

/// A repo registered against a freshly-init'd git repo with no commits
/// must still appear in the persisted manifest. Pre-fix, the empty
/// `HEAD` made `git rev-parse` return exit 128 with "ambiguous
/// argument 'HEAD'", `RepoSource::content_hash` returned `Err`,
/// `persist_manifest` `continue`d on that repo, and the on-disk
/// manifest silently dropped it. The next process restart would
/// re-register from `repos.yaml`, but the manifest snapshot would
/// have a hole in it — exactly the kind of silent drift this fix is
/// supposed to prevent.
#[tokio::test]
async fn add_repo_persists_unborn_head_repo() {
    use crate::federation::manifest::FederationManifest;
    let tmp = tempfile::tempdir().unwrap();
    let manifest_path = tmp.path().join("federation_manifest.bin");
    let fed = FederatedIndex::new(petgraph_backend(&tmp));
    fed.set_manifest_path(Some(manifest_path.clone()));

    // `git init` only — no commit, so HEAD doesn't resolve. This is
    // the real-world case the regression was hidden behind: a user
    // runs `lain server` against an empty checkout.
    let src_dir = tempfile::tempdir().unwrap();
    git2::Repository::init(src_dir.path()).unwrap();
    let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("a").unwrap(), src_dir.path().to_path_buf()).unwrap(),
    );
    fed.add_repo(src, tmp.path()).await.unwrap();

    let loaded = FederationManifest::load_or_default(&manifest_path).unwrap();
    assert_eq!(
        loaded.repos.len(),
        1,
        "a repo with an unborn HEAD must still appear in the persisted manifest; \
         the pre-fix code skipped it because `git rev-parse HEAD` returned exit 128",
    );
    assert_eq!(loaded.repos[0].id.as_str(), "a");
    assert!(
        loaded.repos[0].content_hash.is_empty(),
        "an unborn HEAD has no hash to record; content_hash must be empty, \
         got {:?}",
        loaded.repos[0].content_hash,
    );
}

/// `persist_manifest`'s read-modify-write cycle is serialized by
/// `persist_lock` so two concurrent writers can't each snapshot the
/// membership at slightly different moments and overwrite each
/// other's writes. Pre-fix, an `add_repo` racing with a `remove_repo`
/// would each build a manifest, race to write the temp file + rename,
/// and the rename-atomicity left the on-disk manifest pointing to
/// whichever writer's content committed last — a state that
/// arbitrarily lost a member without any single mutation having
/// caused it. Post-fix, the second writer sees the first's post-
/// mutation state before building its own snapshot, so the final
/// file content matches the final in-memory state.
#[tokio::test]
async fn concurrent_add_and_remove_serialize_persist_manifest() {
    use crate::federation::manifest::FederationManifest;
    let tmp = tempfile::tempdir().unwrap();
    let manifest_path = tmp.path().join("federation_manifest.bin");
    let fed = std::sync::Arc::new(FederatedIndex::new(petgraph_backend(&tmp)));
    fed.set_manifest_path(Some(manifest_path.clone()));

    // Seed a repo. Both racers operate on this baseline.
    let src_dir = tempfile::tempdir().unwrap();
    git2::Repository::init(src_dir.path()).unwrap();
    let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("seed").unwrap(), src_dir.path().to_path_buf())
            .unwrap(),
    );
    fed.add_repo(src, tmp.path()).await.unwrap();

    // Race: a second `add_repo` (for "added") and a `remove_repo` (of
    // the same "added" id) landing at the same instant. Both call
    // `persist_manifest`; the lock forces the second to observe the
    // first's post-mutation membership.
    let added = RepoId::new("added").unwrap();
    let added_for_rm = added.clone();
    let fed_for_add = fed.clone();
    let fed_for_rm = fed.clone();
    let data_dir = tmp.path().to_path_buf();
    let add_task = tokio::spawn(async move {
        let src = tempfile::tempdir().unwrap();
        git2::Repository::init(src.path()).unwrap();
        let s: Box<dyn crate::federation::repo_source::RepoSource> =
            Box::new(WorkspaceDirSource::new(added, src.path().to_path_buf()).unwrap());
        fed_for_add.add_repo(s, &data_dir).await.unwrap();
    });
    let rm_task = tokio::spawn(async move {
        // Fire-and-forget remove; ignore the not-found case if add
        // hadn't yet registered the id.
        let _ = fed_for_rm.remove_repo(&added_for_rm);
    });
    let _ = tokio::join!(add_task, rm_task);

    // Post-condition: the on-disk manifest matches the final
    // in-memory membership. Either `added` is present (if remove
    // happened before add) or absent (if add happened first). What's
    // not acceptable is a torn entry: an `added` row with a
    // half-populated source_config, or a row whose `last_indexed_unix`
    // is older than the membership actually has. The lock
    // guarantees the snapshot is consistent with whatever final
    // membership `list_repos` reports right now.
    let in_mem: std::collections::BTreeMap<String, _> = fed
        .list_repos()
        .into_iter()
        .map(|(id, _)| (id.to_string(), ()))
        .collect();
    let on_disk = FederationManifest::load_or_default(&manifest_path).unwrap();
    let on_disk_ids: std::collections::BTreeSet<_> = on_disk
        .repos
        .iter()
        .map(|e| e.id.as_str().to_string())
        .collect();
    let in_mem_ids: std::collections::BTreeSet<_> = in_mem.keys().cloned().collect();
    assert_eq!(
        on_disk_ids, in_mem_ids,
        "on-disk manifest membership must match in-memory after concurrent \
         add/remove; pre-fix `persist_lock` could let the older snapshot \
         overwrite the newer one. on_disk={:?} in_mem={:?}",
        on_disk_ids, in_mem_ids
    );
}

/// Two concurrent `add_repo` calls for the same `RepoId`. The
/// in-memory `repos` map is keyed by id so the second `insert`
/// overwrites the first; the manifest's `add_repo` is also a
/// single-entry write so the on-disk manifest must end up with
/// exactly one row for that id, not two. Pre-fix `persist_lock`,
/// both writers would snapshot the membership *after their own
/// insert* but before the other's — depending on lock timing the
/// on-disk manifest could end up with a torn entry (a row whose
/// source_config was from one RepoIndex and whose
/// `last_indexed_unix` was from the other), or two rows for the
/// same id if a non-membership-keyed manifest format ever slipped
/// in. Post-fix the lock holds the snapshot against the final
/// in-memory state and the HashMap-keyed insert means duplicates
/// collapse to one row.
#[tokio::test]
async fn concurrent_add_same_id_collapse_to_one_manifest_row() {
    use crate::federation::manifest::FederationManifest;
    let tmp = tempfile::tempdir().unwrap();
    let manifest_path = tmp.path().join("federation_manifest.bin");
    let fed = std::sync::Arc::new(FederatedIndex::new(petgraph_backend(&tmp)));
    fed.set_manifest_path(Some(manifest_path.clone()));

    let id = RepoId::new("dup").unwrap();
    let data_dir = tmp.path().to_path_buf();

    let mut handles = Vec::new();
    for _ in 0..2 {
        let fed = fed.clone();
        let id = id.clone();
        let data_dir = data_dir.clone();
        handles.push(tokio::spawn(async move {
            let src = tempfile::tempdir().unwrap();
            git2::Repository::init(src.path()).unwrap();
            let s: Box<dyn crate::federation::repo_source::RepoSource> =
                Box::new(WorkspaceDirSource::new(id, src.path().to_path_buf()).unwrap());
            fed.add_repo(s, &data_dir).await.unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    let on_disk = FederationManifest::load_or_default(&manifest_path).unwrap();
    let dup_rows: Vec<_> = on_disk
        .repos
        .iter()
        .filter(|e| e.id.as_str() == "dup")
        .collect();
    assert_eq!(
        dup_rows.len(),
        1,
        "two concurrent add_repo for the same id must collapse to one \
         on-disk row, not two; got {} rows",
        dup_rows.len(),
    );
    assert_eq!(
        fed.list_repos().len(),
        1,
        "in-memory membership must also collapse to one entry for the \
         duplicated id, not two",
    );
}

/// Mixed-type concurrent mutations exercise the same lock with a
/// broader call mix than the `add`/`remove` race above. Three adds
/// for distinct ids fire at once — the same shape as the real-world
/// trigger, where a YAML reload can issue a burst of adds without
/// interleaved removes. The post-state must contain every
/// successfully-added id and the seed, and the on-disk manifest
/// must match the final in-memory membership.
///
/// Implementation note: the src `tempfile::tempdir()` for each
/// racer is created in the *outer* scope and only borrowed by the
/// task, not owned by it. If the tempdir were owned by the task
/// closure, it would be dropped when the task returns — at which
/// point the next racer's `persist_manifest` iteration would call
/// `git rev-parse HEAD` against a deleted dir and `content_hash()`
/// would error out with `cannot change to '/tmp/.tmpXXX': No such
/// file`, causing the snapshot to drop the prior racer's row. The
/// bug is in the *fixture*, not the production lock; the
/// pre-existing `concurrent_add_and_remove_serialize_persist_manifest`
/// doesn't trip over it because the lone add-task owns its src
/// tempdir and the rm-task owns no src.
#[tokio::test]
async fn concurrent_mixed_mutations_persist_manifest_consistently() {
    use crate::federation::manifest::FederationManifest;
    let tmp = tempfile::tempdir().unwrap();
    let manifest_path = tmp.path().join("federation_manifest.bin");
    let fed = std::sync::Arc::new(FederatedIndex::new(petgraph_backend(&tmp)));
    fed.set_manifest_path(Some(manifest_path.clone()));

    // Pre-seed one repo so the in-memory map is non-empty at the
    // start; the mixed racers all see the same baseline.
    let seed_dir = tempfile::tempdir().unwrap();
    git2::Repository::init(seed_dir.path()).unwrap();
    let seed_src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("seed").unwrap(), seed_dir.path().to_path_buf())
            .unwrap(),
    );
    fed.add_repo(seed_src, tmp.path()).await.unwrap();

    let ids = ["alpha", "beta", "gamma"];
    let data_dir = tmp.path().to_path_buf();

    // Outer-scope tempdirs: kept alive for the whole test so the
    // post-join persist snapshots can still `git rev-parse` them.
    let mut src_dirs = Vec::new();
    for id_str in ids {
        let d = tempfile::tempdir().unwrap();
        git2::Repository::init(d.path()).unwrap();
        src_dirs.push((id_str.to_string(), d));
    }

    let mut handles = Vec::new();
    for (id_str, d) in &src_dirs {
        let fed = fed.clone();
        let data_dir = data_dir.clone();
        let id = RepoId::new(id_str.as_str()).unwrap();
        let src_path = d.path().to_path_buf();
        handles.push(tokio::spawn(async move {
            let s: Box<dyn crate::federation::repo_source::RepoSource> =
                Box::new(WorkspaceDirSource::new(id, src_path).unwrap());
            fed.add_repo(s, &data_dir).await.unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    let on_disk = FederationManifest::load_or_default(&manifest_path).unwrap();
    let on_disk_ids: std::collections::BTreeSet<_> = on_disk
        .repos
        .iter()
        .map(|e| e.id.as_str().to_string())
        .collect();
    let in_mem_ids: std::collections::BTreeSet<_> = fed
        .list_repos()
        .into_iter()
        .map(|(id, _)| id.to_string())
        .collect();
    let mut expected = std::collections::BTreeSet::new();
    expected.insert("seed".to_string());
    for id in ids {
        expected.insert(id.to_string());
    }
    assert_eq!(
        on_disk_ids, expected,
        "on-disk manifest must contain every successfully-added id and the \
         seed; pre-fix `persist_lock` could let one writer's snapshot miss \
         a concurrent insert. on_disk={:?} expected={:?}",
        on_disk_ids, expected,
    );
    assert_eq!(
        in_mem_ids, expected,
        "in-memory membership must match the expected set after the mixed \
         race; got {:?}",
        in_mem_ids,
    );
    assert_eq!(
        on_disk_ids, in_mem_ids,
        "the lock must keep the on-disk snapshot consistent with the \
         final in-memory state; on_disk={:?} in_mem={:?}",
        on_disk_ids, in_mem_ids,
    );
}
