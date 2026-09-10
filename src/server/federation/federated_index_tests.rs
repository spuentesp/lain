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
use crate::federation::repo_source::WorkspaceDirSource;
use crate::schema::NodeType;
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
