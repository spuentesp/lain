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
#[test]
fn two_repos_with_identical_symbols_get_distinct_ids() {
    use crate::federation::repo_source::WorkspaceDirSource;
    use crate::schema::{GraphNode, NodeType, RepoNamespace};
    let tmp = tempfile::tempdir().unwrap();
    // Two tempdir repos with identical content. We don't index them —
    // the namespace difference is what matters for id uniqueness, and
    // minting the same `(type, path, name, line)` from two namespaces
    // is the contract we're testing.
    let repo_a = tmp.path().join("a");
    let repo_b = tmp.path().join("b");
    std::fs::create_dir_all(&repo_a).unwrap();
    std::fs::create_dir_all(&repo_b).unwrap();
    let src_a = WorkspaceDirSource::new(RepoId::new("a").unwrap(), repo_a.clone()).unwrap();
    let src_b = WorkspaceDirSource::new(RepoId::new("b").unwrap(), repo_b.clone()).unwrap();

    // Use the production constructor (with the repo's namespace)
    // for both. Different repos → different namespaces → different ids.
    let node_a = GraphNode::new_in(
        NodeType::Function,
        "foo".into(),
        "src/lib.rs".into(),
        src_a.id_namespace(),
    );
    let node_b = GraphNode::new_in(
        NodeType::Function,
        "foo".into(),
        "src/lib.rs".into(),
        src_b.id_namespace(),
    );
    assert_ne!(
        node_a.id, node_b.id,
        "two repos with identical symbols must produce distinct ids; \
         the pre-fix code produced equal ids, which made the shared \
         VolatileOverlay collapse them and let id-keyed cleanup in \
         one repo remove the other's symbol"
    );

    // Sanity: identical symbols WITHIN one repo still collide (same
    // path + name + line → same id). That's correct — the namespace
    // is what disambiguates *between* repos, not within one.
    let node_a2 = GraphNode::new_in(
        NodeType::Function,
        "foo".into(),
        "src/lib.rs".into(),
        src_a.id_namespace(),
    );
    assert_eq!(
        node_a.id, node_a2.id,
        "same repo + same symbol → same id (collision within a repo \
         is expected; the id is a uniqueness handle, not a content hash)",
    );

    // The test namespace must not collide with any real namespace.
    let test_node = GraphNode::new(NodeType::Function, "foo".into(), "src/lib.rs".into());
    assert_ne!(node_a.id, test_node.id);
    assert_ne!(node_b.id, test_node.id);
}
