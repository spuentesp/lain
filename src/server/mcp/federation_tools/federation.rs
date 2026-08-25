//! Federation-mode MCP tools that read state out of a `FederatedIndex`:
//! `list_repos`, `get_repo_info`, `get_federation_health`, `search_org`,
//! `get_cross_repo_blast_radius`, `get_cross_repo_blast_radius_for_repo`.
//! The workspace, server-status, and recent-projects tools live in
//! sibling modules under `super`.

use super::dto::{CrossRepoBlastRadius, FederationHealth, RepoInfo, SymbolMatch};
use crate::error::LainError;
use crate::federation::federated_index::FederatedIndex;
use crate::federation::repo_id::{GlobalId, RepoId};
use std::collections::BTreeMap;
use std::ops::Range;

pub fn list_repos(fed: &FederatedIndex) -> Vec<RepoInfo> {
    fed.list_repos()
        .into_iter()
        .map(|(id, health)| {
            let repo = fed.get_repo(&id);
            let (last_refreshed_unix, last_indexed_unix, node_count, edge_count, path) =
                match repo {
                    Some(r) => {
                        let path = r.source().local_path().display().to_string();
                        let last_refreshed = r
                            .source()
                            .last_refreshed()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);
                        let last_indexed = r
                            .last_indexed()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);
                        (
                            last_refreshed,
                            last_indexed,
                            r.nodes().len(),
                            r.edges().len(),
                            path,
                        )
                    }
                    None => (0, 0, 0, 0, String::new()),
                };
            RepoInfo {
                id: id.to_string(),
                path,
                health: health.to_string(),
                last_refreshed_unix,
                last_indexed_unix,
                node_count,
                edge_count,
            }
        })
        .collect()
}

pub fn get_repo_info(fed: &FederatedIndex, id: &RepoId) -> Result<RepoInfo, LainError> {
    let list = list_repos(fed);
    list.into_iter()
        .find(|r| r.id == id.as_str())
        .ok_or_else(|| LainError::NotFound(format!("repo {id}")))
}

pub fn get_federation_health(fed: &FederatedIndex) -> FederationHealth {
    use crate::federation::health::RepoHealth;
    let repos = fed.list_repos();
    let mut h = FederationHealth {
        total_repos: repos.len(),
        ready: 0,
        indexing: 0,
        degraded: 0,
        unavailable: 0,
        missing: 0,
        healthy: false,
        ready_threshold: fed.ready_threshold(),
        total_nodes: fed.backend().node_count(),
        total_edges: fed.backend().edge_count(),
        memory_estimate_bytes: 0,
    };
    for (_, health) in &repos {
        match health {
            RepoHealth::Ready => h.ready += 1,
            RepoHealth::Indexing => h.indexing += 1,
            RepoHealth::Degraded => h.degraded += 1,
            RepoHealth::Unavailable => h.unavailable += 1,
            RepoHealth::Missing => h.missing += 1,
        }
    }
    h.memory_estimate_bytes = (h.total_nodes as u64) * 200 + (h.total_edges as u64) * 100;

    // `docs/REPOS_YAML.md`: "Fraction of repos that must reach `Ready`
    // health before the federation reports `healthy`." An empty
    // federation is healthy — there is nothing failing to be ready.
    h.healthy = if h.total_repos == 0 {
        true
    } else {
        (h.ready as f32 / h.total_repos as f32) >= h.ready_threshold
    };
    h
}

/// Case-insensitive substring search for symbols across every repo in the
/// federation. Matches on `name` or `path`, sorts by `(repo_id, name)`, and
/// truncates to `limit`.
///
/// The primary path iterates `list_repos()` → `get_repo()` → per-repo
/// `RepoIndex::nodes()`, which covers repos added through `add_repo` whether
/// or not they have been projected. A backend fallback then scans
/// `fed.backend().list_nodes()` to catch nodes inserted directly into the
/// federated backend (bypassing `add_repo` / `project_repo` — the same
/// fallback pattern used by `FederatedIndex::resolve_symbol`).
///
/// Results are deduplicated by `(repo_id, name, path)` so a node visible
/// through both the per-repo db (which uses content-hash ids) and the
/// federation backend (which uses deterministic global ids) is returned
/// once — the canonical triple is what callers actually care about.
pub fn search_org(fed: &FederatedIndex, query: &str, limit: usize) -> Vec<SymbolMatch> {
    let q = query.to_lowercase();
    let mut hits: Vec<SymbolMatch> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String, String)> =
        std::collections::HashSet::new();
    let key = |repo: &str, name: &str, path: &str| (repo.to_string(), name.to_string(), path.to_string());

    // Primary path: per-repo nodes.
    for (repo_id, _) in fed.list_repos() {
        if let Some(repo) = fed.get_repo(&repo_id) {
            for n in repo.nodes() {
                if n.name.to_lowercase().contains(&q) || n.path.to_lowercase().contains(&q) {
                    if seen.insert(key(repo_id.as_str(), &n.name, &n.path)) {
                        hits.push(SymbolMatch {
                            global_id: n.id.clone(),
                            repo_id: repo_id.to_string(),
                            name: n.name.clone(),
                            path: n.path.clone(),
                            kind: n.node_type.to_string(),
                        });
                    }
                }
            }
        }
    }

    // Fallback: backend nodes not already collected from per-repo indexes.
    // `repo_id` is parsed from the node's global id, since the backend path
    // has no `list_repos()` iteration to draw from.
    if let Ok(backend_nodes) = fed.backend().list_nodes() {
        for n in backend_nodes {
            let repo_id = GlobalId::parse(&n.id)
                .ok()
                .map(|g| g.repo_id().to_string())
                .unwrap_or_default();
            if seen.contains(&key(&repo_id, &n.name, &n.path)) {
                continue;
            }
            if n.name.to_lowercase().contains(&q) || n.path.to_lowercase().contains(&q) {
                seen.insert(key(repo_id.as_str(), &n.name, &n.path));
                hits.push(SymbolMatch {
                    global_id: n.id.clone(),
                    repo_id,
                    name: n.name.clone(),
                    path: n.path.clone(),
                    kind: n.node_type.to_string(),
                });
            }
        }
    }

    hits.sort_by(|a, b| a.repo_id.cmp(&b.repo_id).then(a.name.cmp(&b.name)));
    hits.truncate(limit);
    hits
}

const BLAST_RADIUS_CAP: usize = 1000;

/// Resolve `symbol` to a single repo via `FederatedIndex::resolve_symbol`,
/// then compute its blast radius. `AmbiguousSymbol` and `NotFound` from the
/// resolver propagate unchanged so callers can prompt the user to disambiguate.
pub fn get_cross_repo_blast_radius(
    fed: &FederatedIndex,
    symbol: &str,
    depth: Range<u32>,
) -> Result<CrossRepoBlastRadius, LainError> {
    let repo_id = fed.resolve_symbol(symbol)?;
    get_cross_repo_blast_radius_for_repo(fed, repo_id.as_str(), symbol, depth)
}

/// Cross-repo blast radius for a specific `(repo_id, symbol)` pair. Useful
/// when `resolve_symbol` would be ambiguous or when the caller already knows
/// which repo owns the seed symbol. Implementation: look up the actual node
/// in the federated backend by name + repo (the caller doesn't know the
/// path component of the global id format), BFS outgoing `Calls` edges
/// through the backend, group results by repo, cap at `BLAST_RADIUS_CAP`.
///
/// Deviation from the brief: the brief's pseudocode built the seed global
/// id with `path = ""`, which never matches a real node (real nodes carry
/// a non-empty path like `src/x.rs`). Looking up the actual node sidesteps
/// that — the rest of the grouping/cap logic is unchanged.
pub fn get_cross_repo_blast_radius_for_repo(
    fed: &FederatedIndex,
    repo_id: &str,
    symbol: &str,
    depth: Range<u32>,
) -> Result<CrossRepoBlastRadius, LainError> {
    use crate::schema::EdgeType;
    let rid = RepoId::new(repo_id)?;
    // Look up the actual node by name + repo so we traverse from a real
    // global id. `backend.find_nodes_by_name` covers both repos added
    // through `add_repo` / `project_repo` and nodes inserted directly
    // into the backend (same fallback pattern `resolve_symbol` uses).
    let seed = fed
        .backend()
        .find_nodes_by_name(symbol)?
        .into_iter()
        .find(|n| match GlobalId::parse(&n.id) {
            Ok(g) => g.repo_id() == rid.as_str(),
            Err(_) => false,
        })
        .ok_or_else(|| {
            LainError::NotFound(format!("symbol {symbol} not found in repo {repo_id}"))
        })?;
    // Blast radius = "what depends on this symbol" = the *callers* of
    // `seed`, not the callees. We traverse incoming `Calls` edges so a
    // blast-radius report answers the question an agent actually asks
    // ("if I change X, what breaks?") rather than the inverse
    // dependency walk. Wishlist #12c fix.
    let traversed = fed.backend().traverse(
        &seed.id,
        EdgeType::Calls,
        depth,
        petgraph::Direction::Incoming,
    )?;
    let mut by_repo: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut total = 0usize;
    let mut truncated = false;
    for n in traversed {
        if total >= BLAST_RADIUS_CAP {
            truncated = true;
            break;
        }
        if let Ok(gid) = GlobalId::parse(&n.id) {
            by_repo.entry(gid.repo_id().to_string()).or_default().push(n.id.clone());
        }
        total += 1;
    }
    Ok(CrossRepoBlastRadius { by_repo, total_count: total, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::federated_index::FederatedIndex;
    use crate::federation::graph_backend::PetgraphBackend;
    use crate::federation::health::RepoHealth;
    use crate::federation::repo_id::RepoId;
    use crate::federation::repo_source::WorkspaceDirSource;
    use std::sync::Arc;

    #[tokio::test]
    async fn list_repos_returns_registered() {
        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        // `RepoIndex::new` instantiates a `GitSensor` against the source's
        // local path, so the path must be a real git repo. `/tmp` is not;
        // initialize a throwaway repo in a fresh tempdir for the source.
        // (Same fix used by `federated_index_tests::add_repo_registers_and_lists_it`.)
        let src_dir = tempfile::tempdir().unwrap();
        git2::Repository::init(src_dir.path()).unwrap();
        let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
            WorkspaceDirSource::new(
                RepoId::new("a").unwrap(),
                src_dir.path().to_path_buf(),
            )
            .unwrap(),
        );
        fed.add_repo(src, tmp.path()).await.unwrap();
        let list = list_repos(&fed);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "a");
        // `RepoInfo::health` is a `String`; compare against the canonical
        // string form of `RepoHealth::Indexing`.
        assert_eq!(list[0].health, RepoHealth::Indexing.as_str());
        // Sanity-check the path was projected through.
        assert_eq!(list[0].path, src_dir.path().display().to_string());
    }

    #[tokio::test]
    async fn get_repo_info_returns_matching_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        let src_dir = tempfile::tempdir().unwrap();
        git2::Repository::init(src_dir.path()).unwrap();
        let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
            WorkspaceDirSource::new(
                RepoId::new("a").unwrap(),
                src_dir.path().to_path_buf(),
            )
            .unwrap(),
        );
        fed.add_repo(src, tmp.path()).await.unwrap();
        let info = get_repo_info(&fed, &RepoId::new("a").unwrap()).unwrap();
        assert_eq!(info.id, "a");
        assert_eq!(info.health, RepoHealth::Indexing.as_str());
    }

    #[tokio::test]
    async fn get_repo_info_unknown_id_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        let err = get_repo_info(&fed, &RepoId::new("ghost").unwrap()).unwrap_err();
        assert!(matches!(err, LainError::NotFound(_)));
    }

    #[test]
    fn list_repos_empty_index_returns_empty_vec() {
        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        assert!(list_repos(&fed).is_empty());
    }

    #[test]
    fn federation_health_counts_correctly() {
        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        let h = get_federation_health(&fed);
        assert_eq!(h.total_repos, 0);
        assert_eq!(h.total_nodes, 0);
        assert_eq!(h.total_edges, 0);
    }

    #[tokio::test]
    async fn search_org_finds_across_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        fed.backend().upsert_node_global("repo-a:Function:src/auth.rs:verify_token", crate::schema::NodeType::Function, "src/auth.rs", "verify_token").unwrap();
        fed.backend().upsert_node_global("repo-b:Function:src/auth.rs:verify_token", crate::schema::NodeType::Function, "src/auth.rs", "verify_token").unwrap();
        fed.backend().upsert_node_global("repo-c:Function:src/x.rs:other", crate::schema::NodeType::Function, "src/x.rs", "other").unwrap();
        let hits = search_org(&fed, "verify", 10);
        assert_eq!(hits.len(), 2);
        let repos: std::collections::HashSet<_> = hits.iter().map(|h| h.repo_id.clone()).collect();
        assert!(repos.contains("repo-a"));
        assert!(repos.contains("repo-b"));
    }

    #[tokio::test]
    async fn cross_repo_blast_radius_groups_by_repo() {
        // Brief deviation (see task-18-report.md): the brief's original
        // assertions expected `by_repo.get("repo-a") == 1` and
        // `by_repo.get("repo-b") == 1`, but `GraphBackend::traverse` is
        // outgoing-only and excludes the seed, while the brief's edge goes
        // INTO the seed (`caller_of_shared --Calls--> shared`). With the
        // outgoing-only seed semantics, the seed has no outgoing `Calls`
        // edges in this fixture, so the result is correctly empty. The
        // assertions below document that behavior; a second test
        // (`cross_repo_blast_radius_outgoing_edges_group_by_repo`) exercises
        // the actual grouping path.
        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        fed.backend().upsert_node_global("repo-a:Function:src/x.rs:shared", crate::schema::NodeType::Function, "src/x.rs", "shared").unwrap();
        fed.backend().upsert_node_global("repo-b:Function:src/x.rs:shared", crate::schema::NodeType::Function, "src/x.rs", "shared").unwrap();
        fed.backend().upsert_node_global("repo-b:Function:src/y.rs:caller_of_shared", crate::schema::NodeType::Function, "src/y.rs", "caller_of_shared").unwrap();
        fed.backend().upsert_edge(crate::schema::GraphEdge::new(
            crate::schema::EdgeType::Calls,
            "repo-b:Function:src/y.rs:caller_of_shared".into(),
            "repo-b:Function:src/x.rs:shared".into(),
        )).unwrap();
        let result = get_cross_repo_blast_radius_for_repo(&fed, "repo-a", "shared", 1..3).unwrap();
        assert_eq!(result.by_repo.get("repo-a").map(|v| v.len()).unwrap_or(0), 0);
        assert_eq!(result.by_repo.get("repo-b").map(|v| v.len()).unwrap_or(0), 0);
        assert_eq!(result.total_count, 0);
        assert!(!result.truncated);
    }

    #[tokio::test]
    async fn cross_repo_blast_radius_incoming_edges_group_by_repo() {
        // Wishlist #12c fix: blast radius now traverses INCOMING
        // `Calls` edges (callers of the seed), not outgoing. The
        // fixture is identical shape to the pre-fix outgoing test
        // but the expected `by_repo` mapping is the *callers* of
        // the seed, not the callees. Includes:
        //   - one direct caller in repo-a (depth 1, other_caller)
        //   - one transitive caller in repo-a (depth 2 via other_caller)
        //   - one INCOMING edge to repo-b's `shared` from `caller_of_shared`
        //     (depth 1, repo-b bucket) — different node id, but the
        //     traverse follows the edge since it points at `shared`
        //   - one OUTGOING edge in repo-a from `shared` to `self_call`
        //     — must be ignored (blast radius is callers, not callees)
        //   - one OUTGOING edge in repo-a from `shared` to
        //     `direct_consumer` — must be ignored
        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        let backend = fed.backend();
        // Nodes
        backend.upsert_node_global("repo-a:Function:src/x.rs:shared", crate::schema::NodeType::Function, "src/x.rs", "shared").unwrap();
        backend.upsert_node_global("repo-b:Function:src/x.rs:shared", crate::schema::NodeType::Function, "src/x.rs", "shared").unwrap();
        backend.upsert_node_global("repo-b:Function:src/y.rs:direct_consumer", crate::schema::NodeType::Function, "src/y.rs", "direct_consumer").unwrap();
        backend.upsert_node_global("repo-b:Function:src/z.rs:transitive_consumer", crate::schema::NodeType::Function, "src/z.rs", "transitive_consumer").unwrap();
        backend.upsert_node_global("repo-a:Function:src/x.rs:self_call", crate::schema::NodeType::Function, "src/x.rs", "self_call").unwrap();
        backend.upsert_node_global("repo-a:Function:src/w.rs:other_caller", crate::schema::NodeType::Function, "src/w.rs", "other_caller").unwrap();
        backend.upsert_node_global("repo-a:Function:src/v.rs:transitive_caller", crate::schema::NodeType::Function, "src/v.rs", "transitive_caller").unwrap();
        backend.upsert_node_global("repo-b:Function:src/y.rs:caller_of_shared", crate::schema::NodeType::Function, "src/y.rs", "caller_of_shared").unwrap();
        // Outgoing noise — must be ignored (blast radius is callers):
        backend.upsert_edge(crate::schema::GraphEdge::new(
            crate::schema::EdgeType::Calls,
            "repo-a:Function:src/x.rs:shared".into(),
            "repo-b:Function:src/y.rs:direct_consumer".into(),
        )).unwrap();
        backend.upsert_edge(crate::schema::GraphEdge::new(
            crate::schema::EdgeType::Calls,
            "repo-a:Function:src/x.rs:shared".into(),
            "repo-a:Function:src/x.rs:self_call".into(),
        )).unwrap();
        // Incoming edges to the seed (repo-a's `shared`):
        backend.upsert_edge(crate::schema::GraphEdge::new(
            crate::schema::EdgeType::Calls,
            "repo-a:Function:src/w.rs:other_caller".into(),
            "repo-a:Function:src/x.rs:shared".into(),
        )).unwrap();
        // Incoming edge to repo-b's `shared`:
        backend.upsert_edge(crate::schema::GraphEdge::new(
            crate::schema::EdgeType::Calls,
            "repo-b:Function:src/y.rs:caller_of_shared".into(),
            "repo-b:Function:src/x.rs:shared".into(),
        )).unwrap();
        // Transitive caller (depth 2) of repo-a's `shared`:
        backend.upsert_edge(crate::schema::GraphEdge::new(
            crate::schema::EdgeType::Calls,
            "repo-a:Function:src/v.rs:transitive_caller".into(),
            "repo-a:Function:src/w.rs:other_caller".into(),
        )).unwrap();

        // Resolve via repo-a's `shared` (the seed). Callers at depths
        // 1 and 2: other_caller (depth 1, repo-a), transitive_caller
        // (depth 2 via other_caller, repo-a). caller_of_shared is a
        // caller of repo-b's `shared` (a DIFFERENT global id), not of
        // repo-a's, so it must NOT appear in the seed's blast radius.
        let result = get_cross_repo_blast_radius_for_repo(&fed, "repo-a", "shared", 1..3).unwrap();
        // Seed at depth 0 is excluded by `traverse` (min_depth=1).
        // `by_repo` should reflect *callers* of repo-a's `shared`:
        //   - repo-a: other_caller (direct) + transitive_caller (via
        //     other_caller) = 2
        assert_eq!(result.by_repo.get("repo-a").map(|v| v.len()).unwrap_or(0), 2);
        // repo-b has no callers of repo-a's `shared` (caller_of_shared
        // points at repo-b's `shared`, which is a different global id).
        assert_eq!(
            result.by_repo.get("repo-b").map(|v| v.len()).unwrap_or(0),
            0
        );
        assert_eq!(result.total_count, 2);
        assert!(!result.truncated);
        // Sanity-check the actual node ids in each bucket.
        let repo_a_ids: std::collections::HashSet<_> = result
            .by_repo
            .get("repo-a")
            .map(|v| v.iter().cloned().collect())
            .unwrap_or_default();
        assert!(repo_a_ids.contains("repo-a:Function:src/w.rs:other_caller"));
        assert!(repo_a_ids.contains("repo-a:Function:src/v.rs:transitive_caller"));
        // The pre-fix outgoing-direction result was direct_consumer
        // and self_call. Those must NOT appear in the by_repo buckets
        // — blast radius is callers, not callees.
        assert!(!result
            .by_repo
            .values()
            .any(|v| v.iter().any(|id| id.contains("direct_consumer"))));
        assert!(!result
            .by_repo
            .values()
            .any(|v| v.iter().any(|id| id.contains("self_call"))));
        // caller_of_shared is a caller of repo-b's `shared`, not of
        // repo-a's. It must not leak into the seed's blast radius.
        assert!(!result
            .by_repo
            .values()
            .any(|v| v.iter().any(|id| id.contains("caller_of_shared"))));
    }

    #[tokio::test]
    async fn cross_repo_blast_radius_resolves_via_resolve_symbol() {
        // The non-`_for_repo` variant calls `resolve_symbol` first. With a
        // single repo owning `lonely`, resolve_symbol returns its RepoId
        // and the function continues. We add an INCOMING edge so the
        // result is non-empty (blast radius is callers, post #12c).
        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        let backend = fed.backend();
        // Bypass `add_repo` / `project_repo` (which would also work, but
        // requires a git source dir). `resolve_symbol` has a fallback to
        // `backend.find_nodes_by_name`, so direct backend insertion is
        // enough to populate the symbol index for this test.
        backend.upsert_node_global("repo-only:Function:src/x.rs:lonely", crate::schema::NodeType::Function, "src/x.rs", "lonely").unwrap();
        backend.upsert_node_global("repo-only:Function:src/y.rs:caller_of_lonely", crate::schema::NodeType::Function, "src/y.rs", "caller_of_lonely").unwrap();
        backend.upsert_edge(crate::schema::GraphEdge::new(
            crate::schema::EdgeType::Calls,
            "repo-only:Function:src/y.rs:caller_of_lonely".into(),
            "repo-only:Function:src/x.rs:lonely".into(),
        )).unwrap();
        let result = get_cross_repo_blast_radius(&fed, "lonely", 1..3).unwrap();
        assert_eq!(result.by_repo.get("repo-only").map(|v| v.len()).unwrap_or(0), 1);
        assert_eq!(result.total_count, 1);
    }
}

#[cfg(test)]
mod ready_threshold_tests {
    use super::*;
    use crate::federation::graph_backend::PetgraphBackend;

    fn empty_fed() -> (tempfile::TempDir, FederatedIndex) {
        let tmp = tempfile::tempdir().unwrap();
        let backend = std::sync::Arc::new(PetgraphBackend::new(tmp.path()).unwrap());
        (tmp, FederatedIndex::new(backend))
    }

    /// `repos.yaml`'s `ready_threshold` is documented as the fraction of
    /// repos that must be `Ready` "before the federation reports
    /// `healthy`". Nothing read the setting, and `FederationHealth` had
    /// no `healthy` field for it to report through, so the documented
    /// behaviour did not exist at all.
    #[test]
    fn the_configured_threshold_is_reported_and_defaults_sanely() {
        let (_tmp, fed) = empty_fed();
        assert_eq!(
            fed.ready_threshold(),
            crate::federation::config::DEFAULT_READY_THRESHOLD
        );

        let h = get_federation_health(&fed);
        assert_eq!(h.ready_threshold, crate::federation::config::DEFAULT_READY_THRESHOLD);
        assert!(h.healthy, "an empty federation has nothing failing to be ready");
    }

    #[test]
    fn the_threshold_is_settable_and_clamped() {
        let (_tmp, fed) = empty_fed();
        fed.set_ready_threshold(0.5);
        assert_eq!(fed.ready_threshold(), 0.5);
        assert_eq!(get_federation_health(&fed).ready_threshold, 0.5);

        // A nonsense value in `repos.yaml` must not make `healthy`
        // unreachable or trivially true.
        fed.set_ready_threshold(4.2);
        assert_eq!(fed.ready_threshold(), 1.0);
        fed.set_ready_threshold(-1.0);
        assert_eq!(fed.ready_threshold(), 0.0);
    }

    /// The loader must install the configured value, or the knob is
    /// still decorative no matter what the health tool reports.
    #[test]
    fn the_loader_installs_the_configured_threshold() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src/server/federation/loader.rs"),
        )
        .unwrap();
        assert!(
            src.contains("set_ready_threshold(config.ready_threshold)"),
            "the loader must push `repos.yaml`'s ready_threshold into the federation"
        );
    }
}
