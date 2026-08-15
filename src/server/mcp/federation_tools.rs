//! Federation-mode MCP tools.
//!
//! Exposes six read-only tools for inspecting the live state of a
//! `FederatedIndex`:
//!
//! - `list_repos` — returns every registered repo with its current health,
//!   path, last refresh/index timestamps, and node/edge counts.
//! - `get_repo_info` — returns the same `RepoInfo` payload for a single
//!   repo by id.
//! - `get_federation_health` — returns aggregate counts per health bucket
//!   plus total node/edge counts and a rough memory estimate.
//! - `search_org` — case-insensitive substring search across every repo's
//!   symbols (matched on `name` or `path`), sorted by `(repo_id, name)` and
//!   truncated to a caller-supplied limit.
//! - `get_cross_repo_blast_radius` — the headline tool. Resolves a symbol
//!   across the federation, then traverses outgoing `Calls` edges in the
//!   federated graph and groups the visited nodes by repo.
//! - `get_cross_repo_blast_radius_for_repo` — same as above but the caller
//!   disambiguates the repo explicitly, bypassing `resolve_symbol`.
//!
//! All tools are gated on the MCP server having been constructed with a
//! `FederatedIndex` (see `LainMcpServer::with_federation`). When the server
//! runs in single-workspace mode these tools are not registered and the
//! existing tool surface is unchanged.

use crate::error::LainError;
use crate::federation::federated_index::FederatedIndex;
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::server::LainServer;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoInfo {
    pub id: String,
    pub path: String,
    pub health: String,
    pub last_refreshed_unix: i64,
    pub last_indexed_unix: i64,
    pub node_count: usize,
    pub edge_count: usize,
}

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FederationHealth {
    pub total_repos: usize,
    pub ready: usize,
    pub indexing: usize,
    pub degraded: usize,
    pub unavailable: usize,
    pub missing: usize,
    pub total_nodes: usize,
    pub total_edges: usize,
    pub memory_estimate_bytes: u64,
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
    h
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SymbolMatch {
    pub global_id: String,
    pub repo_id: String,
    pub name: String,
    pub path: String,
    pub kind: String,
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
/// fallback pattern used by `FederatedIndex::resolve_symbol`). Results are
/// deduplicated by `global_id` so a node visible through both paths is
/// returned once.
pub fn search_org(fed: &FederatedIndex, query: &str, limit: usize) -> Vec<SymbolMatch> {
    let q = query.to_lowercase();
    let mut hits: Vec<SymbolMatch> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Primary path: per-repo nodes.
    for (repo_id, _) in fed.list_repos() {
        if let Some(repo) = fed.get_repo(&repo_id) {
            for n in repo.nodes() {
                if n.name.to_lowercase().contains(&q) || n.path.to_lowercase().contains(&q) {
                    if seen.insert(n.id.clone()) {
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
            if seen.contains(&n.id) {
                continue;
            }
            if n.name.to_lowercase().contains(&q) || n.path.to_lowercase().contains(&q) {
                let repo_id = GlobalId::parse(&n.id)
                    .ok()
                    .map(|g| g.repo_id().to_string())
                    .unwrap_or_default();
                seen.insert(n.id.clone());
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

/// Result of a cross-repo blast radius traversal: every node reachable from
/// the seed via outgoing `Calls` edges in `[min_depth, max_depth)`, grouped
/// by the repo each node came from. `total_count` is the number of nodes we
/// tried to bucket (including any whose global id failed to parse, which
/// silently fall out of `by_repo`). `truncated` is `true` when the result
/// hit the per-call cap of 1000 nodes — additional reachable nodes exist
/// beyond it but were not loaded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CrossRepoBlastRadius {
    pub by_repo: BTreeMap<String, Vec<String>>,
    pub total_count: usize,
    pub truncated: bool,
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
    let traversed = fed.backend().traverse(&seed.id, EdgeType::Calls, depth)?;
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
    async fn cross_repo_blast_radius_outgoing_edges_group_by_repo() {
        // Companion to `cross_repo_blast_radius_groups_by_repo`. Builds a
        // fixture where the seed has OUTGOING `Calls` edges, so traverse
        // actually visits nodes and exercises the by-repo grouping. Includes:
        //   - one direct consumer in repo-b (depth 1)
        //   - one transitive consumer in repo-b (depth 2 via direct_consumer)
        //   - one outgoing Calls edge in repo-a from `shared` to itself
        //     (`self_call`) — confirms by_repo buckets are repo-correct
        //   - one INCOMING edge to the seed from repo-a's `other_caller` —
        //     traverse is outgoing-only, so this must be ignored
        //   - one INCOMING edge to repo-b's `shared` from repo-b's
        //     `caller_of_shared` — must be ignored (different seed id)
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
        backend.upsert_node_global("repo-b:Function:src/y.rs:caller_of_shared", crate::schema::NodeType::Function, "src/y.rs", "caller_of_shared").unwrap();
        // Outgoing edges from the seed (repo-a's `shared`):
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
        // Outgoing edge from direct_consumer (depth 2):
        backend.upsert_edge(crate::schema::GraphEdge::new(
            crate::schema::EdgeType::Calls,
            "repo-b:Function:src/y.rs:direct_consumer".into(),
            "repo-b:Function:src/z.rs:transitive_consumer".into(),
        )).unwrap();
        // Incoming noise — must be ignored:
        backend.upsert_edge(crate::schema::GraphEdge::new(
            crate::schema::EdgeType::Calls,
            "repo-a:Function:src/w.rs:other_caller".into(),
            "repo-a:Function:src/x.rs:shared".into(),
        )).unwrap();
        backend.upsert_edge(crate::schema::GraphEdge::new(
            crate::schema::EdgeType::Calls,
            "repo-b:Function:src/y.rs:caller_of_shared".into(),
            "repo-b:Function:src/x.rs:shared".into(),
        )).unwrap();

        let result = get_cross_repo_blast_radius_for_repo(&fed, "repo-a", "shared", 1..3).unwrap();
        // Seed at depth 0 is excluded by `traverse` (min_depth=1).
        // Visited at depths 1 and 2: direct_consumer, self_call,
        // transitive_consumer. Bucket per repo:
        assert_eq!(result.by_repo.get("repo-a").map(|v| v.len()).unwrap_or(0), 1);
        assert_eq!(result.by_repo.get("repo-b").map(|v| v.len()).unwrap_or(0), 2);
        assert_eq!(result.total_count, 3);
        assert!(!result.truncated);
        // Sanity-check the actual node ids in each bucket.
        let repo_a_ids: std::collections::HashSet<_> = result
            .by_repo
            .get("repo-a")
            .map(|v| v.iter().cloned().collect())
            .unwrap_or_default();
        let repo_b_ids: std::collections::HashSet<_> = result
            .by_repo
            .get("repo-b")
            .map(|v| v.iter().cloned().collect())
            .unwrap_or_default();
        assert!(repo_a_ids.contains("repo-a:Function:src/x.rs:self_call"));
        assert!(repo_b_ids.contains("repo-b:Function:src/y.rs:direct_consumer"));
        assert!(repo_b_ids.contains("repo-b:Function:src/z.rs:transitive_consumer"));
    }

    #[tokio::test]
    async fn cross_repo_blast_radius_resolves_via_resolve_symbol() {
        // The non-`_for_repo` variant calls `resolve_symbol` first. With a
        // single repo owning `lonely`, resolve_symbol returns its RepoId
        // and the function continues. We add an outgoing edge so the
        // result is non-empty.
        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        let backend = fed.backend();
        // Bypass `add_repo` / `project_repo` (which would also work, but
        // requires a git source dir). `resolve_symbol` has a fallback to
        // `backend.find_nodes_by_name`, so direct backend insertion is
        // enough to populate the symbol index for this test.
        backend.upsert_node_global("repo-only:Function:src/x.rs:lonely", crate::schema::NodeType::Function, "src/x.rs", "lonely").unwrap();
        backend.upsert_node_global("repo-only:Function:src/y.rs:consumer", crate::schema::NodeType::Function, "src/y.rs", "consumer").unwrap();
        backend.upsert_edge(crate::schema::GraphEdge::new(
            crate::schema::EdgeType::Calls,
            "repo-only:Function:src/x.rs:lonely".into(),
            "repo-only:Function:src/y.rs:consumer".into(),
        )).unwrap();
        let result = get_cross_repo_blast_radius(&fed, "lonely", 1..3).unwrap();
        assert_eq!(result.by_repo.get("repo-only").map(|v| v.len()).unwrap_or(0), 1);
        assert_eq!(result.total_count, 1);
    }
}

// =============================================================================
// Workspace tools (read-only)
// =============================================================================
//
// Three new MCP tools for workspace-aware federation. All are read-only.
// They depend on the WorkspacesFile the server was constructed with — the
// handler passes the active workspace file at dispatch time.

use crate::federation::workspace::{WorkspaceSourceConfig, WorkspacesFile};
use crate::state::ActiveWorkspace;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceInfo {
    pub name: String,
    pub description: Option<String>,
    /// Source kind as a stable label: "workspace_dir" or "workspace_clone".
    /// None if the workspace was declared without a `source:` block.
    pub source: Option<String>,
    pub member_count: usize,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActiveWorkspaceInfo {
    pub name: String,
    pub members: Vec<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceRepoInfo {
    pub repo_id: String,
    pub path: String,
    pub health: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceDetail {
    pub name: String,
    pub description: Option<String>,
    pub source: Option<String>,
    pub members: Vec<WorkspaceRepoInfo>,
}

fn source_label(s: &Option<WorkspaceSourceConfig>) -> Option<String> {
    s.as_ref().map(|c| match c {
        WorkspaceSourceConfig::WorkspaceDir { .. } => "workspace_dir".to_string(),
        WorkspaceSourceConfig::WorkspaceClone { .. } => "workspace_clone".to_string(),
    })
}

pub fn list_workspaces(
    workspaces: &WorkspacesFile,
    active: Option<&ActiveWorkspace>,
) -> Vec<WorkspaceInfo> {
    workspaces.workspaces.iter().map(|ws| {
        let is_active = active.as_ref().map(|a| a.name == ws.name).unwrap_or(false);
        WorkspaceInfo {
            name: ws.name.clone(),
            description: ws.description.clone(),
            source: source_label(&ws.source),
            member_count: ws.members.len(),
            is_active,
        }
    }).collect()
}

/// Identify the workspace whose member set exactly matches the loaded repo
/// ids in the federation. Returns `LainError::Workspace(...)` if no
/// workspace matches (no workspace was active, or the federation was
/// loaded without workspace filtering).
pub fn get_active_workspace(
    fed: &FederatedIndex,
    workspaces: &WorkspacesFile,
) -> Result<ActiveWorkspaceInfo, LainError> {
    let loaded: std::collections::HashSet<String> =
        fed.list_repos().into_iter().map(|(id, _)| id.to_string()).collect();
    if loaded.is_empty() {
        return Err(LainError::Workspace(
            "no repos loaded; no active workspace".into(),
        ));
    }
    let active = workspaces.workspaces.iter()
        .find(|ws| {
            let ws_set: std::collections::HashSet<&String> = ws.members.iter().collect();
            ws_set.len() == loaded.len()
                && ws_set.iter().all(|m| loaded.contains(*m))
                && loaded.iter().all(|l| ws_set.contains(l))
        })
        .ok_or_else(|| LainError::Workspace(
            "federation loaded but no workspace matches the loaded repos".into(),
        ))?;
    Ok(ActiveWorkspaceInfo {
        name: active.name.clone(),
        members: active.members.clone(),
        source: source_label(&active.source),
    })
}

pub fn get_workspace(
    fed: &FederatedIndex,
    workspaces: &WorkspacesFile,
    name: &str,
) -> Result<WorkspaceDetail, LainError> {
    let ws = workspaces.workspaces.iter().find(|w| w.name == name)
        .ok_or_else(|| LainError::NotFound(format!("workspace {name}")))?;
    // Resolve path + health for each member from the federation, if loaded.
    let loaded = fed.list_repos();
    let mut members = Vec::with_capacity(ws.members.len());
    for m in &ws.members {
        let info = loaded.iter().find(|(id, _)| id.as_str() == m);
        let (path, health) = match info {
            Some((id, h)) => {
                let repo = fed.get_repo(id);
                let path = repo.map(|r| r.source().local_path().display().to_string()).unwrap_or_default();
                (path, h.to_string())
            }
            None => (String::new(), "not_loaded".to_string()),
        };
        members.push(WorkspaceRepoInfo {
            repo_id: m.clone(),
            path,
            health,
        });
    }
    Ok(WorkspaceDetail {
        name: ws.name.clone(),
        description: ws.description.clone(),
        source: source_label(&ws.source),
        members,
    })
}

// =============================================================================
// Per-workspace graph (for the dashboard's D3 force-directed view)
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphNode {
    pub id: String,
    pub name: String,
    pub path: String,
    pub repo_id: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    pub edge_type: String,
    pub cross_repo: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceGraph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub truncated: bool,
}

const GRAPH_NODE_CAP: usize = 5000;
const GRAPH_EDGE_CAP: usize = 10000;

fn node_kind_str(s: &str) -> bool {
    matches!(s, "Function" | "Method" | "Class")
}

fn edge_kind_str(s: &str) -> bool {
    matches!(s, "Calls" | "Imports")
}

/// Per-workspace graph data for the dashboard's D3 force-directed view.
///
/// Filters to `Function` / `Method` / `Class` nodes and `Calls` / `Imports`
/// edges (per the spec's "filtered Functions + Calls + cross-repo" scope).
/// Marks edges as `cross_repo: true` when source's repo_id differs from
/// target's. Caps at 5000 nodes / 10000 edges.
pub fn get_workspace_graph(
    fed: &FederatedIndex,
    workspaces: &WorkspacesFile,
    filter: Option<&str>,
) -> Result<WorkspaceGraph, LainError> {
    // Identify the active workspace by intersecting loaded repos with
    // each workspace's member set. Errors if no match.
    let loaded: std::collections::HashSet<String> =
        fed.list_repos().into_iter().map(|(id, _)| id.to_string()).collect();
    let active = workspaces.workspaces.iter()
        .find(|ws| {
            let ws_set: std::collections::HashSet<&String> = ws.members.iter().collect();
            ws_set.len() == loaded.len()
                && ws_set.iter().all(|m| loaded.contains(*m))
                && loaded.iter().all(|l| ws_set.contains(l))
        })
        .ok_or_else(|| LainError::Workspace(
            "federation loaded but no workspace matches the loaded repos".into(),
        ))?;
    let members: std::collections::HashSet<String> = active.members.iter().cloned().collect();

    let all_nodes = fed.backend().list_nodes().map_err(LainError::from)?;
    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut truncated = false;
    for n in all_nodes {
        let kind = format!("{:?}", n.node_type);
        if !node_kind_str(&kind) { continue; }
        let gid = crate::federation::repo_id::GlobalId::parse(&n.id).ok();
        let repo_id = gid.as_ref().map(|g| g.repo_id().to_string()).unwrap_or_default();
        if !members.contains(&repo_id) { continue; }
        if let Some(f) = filter {
            if !n.name.contains(f) && !n.path.contains(f) { continue; }
        }
        if nodes.len() >= GRAPH_NODE_CAP {
            truncated = true;
            break;
        }
        nodes.push(GraphNode {
            id: n.id.clone(),
            name: n.name.clone(),
            path: n.path.clone(),
            repo_id,
            kind,
        });
    }
    let node_ids: std::collections::HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();

    let mut edges: Vec<GraphEdge> = Vec::new();
    let all_edges = fed.backend().all_edges().map_err(LainError::from)?;
    for e in all_edges {
        if !node_ids.contains(e.source_id.as_str()) || !node_ids.contains(e.target_id.as_str()) { continue; }
        let kind = format!("{:?}", e.edge_type);
        if !edge_kind_str(&kind) { continue; }
        if edges.len() >= GRAPH_EDGE_CAP {
            truncated = true;
            break;
        }
        let s = crate::federation::repo_id::GlobalId::parse(&e.source_id).ok();
        let t = crate::federation::repo_id::GlobalId::parse(&e.target_id).ok();
        let cross_repo = match (s, t) {
            (Some(a), Some(b)) => a.repo_id() != b.repo_id(),
            _ => false,
        };
        edges.push(GraphEdge {
            source: e.source_id,
            target: e.target_id,
            edge_type: kind,
            cross_repo,
        });
    }

    Ok(WorkspaceGraph { nodes, edges, truncated })
}

// =============================================================================
// Server-status / recent-projects tools (always available)
// =============================================================================
//
// These tools report on the server's own state, not on a federation's
// contents. They are registered unconditionally in the MCP `tools/list`
// response (alongside the registry's own tools) regardless of whether
// the server is running in federation mode.

/// Format `t` as seconds-since-UNIX-epoch. Used by `get_server_status`
/// for `started_at` / `last_sync_at`. Returns 0 for pre-epoch timestamps
/// rather than panicking, since `SystemTime` subtraction is saturating.
fn system_time_to_unix(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Render the per-process server status payload consumed by the
/// dashboard's status bar.
///
/// Fields:
/// - `pid`: the process id (from `std::process::id`)
/// - `transport`: "stdio" or "http", or null when the server is in
///   single-workspace mode (no MCP transport active)
/// - `port`: TCP port for HTTP transport; null otherwise
/// - `started_at`, `last_sync_at`: seconds since UNIX epoch
/// - `last_error`: most recent sync error message, or null
/// - `repo_count`, `workspace_count`: live counts from the federation
pub fn get_server_status(server: &LainServer) -> serde_json::Value {
    let transport = server.transport().map(|t| match t {
        crate::server::Transport::Stdio => "stdio".to_string(),
        crate::server::Transport::Http => "http".to_string(),
    });
    serde_json::json!({
        "pid": std::process::id(),
        "transport": transport,
        "port": server.port(),
        "started_at": system_time_to_unix(server.started_at()),
        "last_sync_at": system_time_to_unix(server.last_sync_at()),
        "last_error": server.last_error(),
        "repo_count": server.repo_count(),
        "workspace_count": server.workspace_count(),
    })
}

/// Project metadata enriched with repo/workspace counts from the
/// project's `repos.yaml` / `workspaces.yaml`. Returned as one entry
/// per row in the `list_recent_projects` response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecentProjectEntry {
    pub path: PathBuf,
    pub last_used: i64,
    pub workspace_count: usize,
    pub repo_count: usize,
}

/// Compute the workspace + repo counts for a recent project entry
/// based on its `repos.yaml` / `workspaces.yaml` paths. Failures
/// (missing files, parse errors) collapse to zero counts so a single
/// broken entry never blocks the whole list.
fn counts_for_project(repos_yaml: &Path) -> (usize, usize) {
    let cfg = crate::federation::config::FederationConfig::load(repos_yaml).ok();
    let repo_count = cfg.as_ref().map(|c| c.repos.len()).unwrap_or(0);
    let ws_path = repos_yaml
        .parent()
        .map(|p| p.join("workspaces.yaml"));
    let workspace_count = ws_path
        .as_ref()
        .and_then(|p| crate::federation::workspace::WorkspacesFile::load(p).ok())
        .map(|w| w.workspaces.len())
        .unwrap_or(0);
    (workspace_count, repo_count)
}

/// Build the `list_recent_projects` response. Each entry combines a
/// recent-projects record with live repo/workspace counts from the
/// referenced `repos.yaml` (and `workspaces.yaml` next to it).
pub fn list_recent_projects() -> Result<Vec<RecentProjectEntry>, LainError> {
    let raw = crate::config::recent_projects::list()
        .map_err(|e| LainError::Other(format!("recent_projects::list: {e}")))?;
    Ok(raw
        .into_iter()
        .map(|r| {
            let (workspace_count, repo_count) = counts_for_project(&r.path);
            RecentProjectEntry {
                path: r.path,
                last_used: r.last_used,
                workspace_count,
                repo_count,
            }
        })
        .collect())
}

#[cfg(test)]
mod server_status_tests {
    use super::*;
    use crate::server::LainConfig;
    use std::path::PathBuf;

    /// Round-trip the live server through `get_server_status` and assert
    /// the shape. Uses `LainServer::new` (single-workspace mode) so the
    /// test doesn't need a federation fixture; the fields that vary by
    /// mode (transport, port, repo_count, workspace_count) are checked
    /// for null / zero rather than asserting concrete values.
    #[test]
    fn get_server_status_returns_expected_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let mem = tmp.path().join("graph.bin");
        // `LainServer::new` requires `workspace` to be a git repo so the
        // GitSensor can attach. Initialize one.
        git2::Repository::init(&ws).unwrap();
        let server = LainServer::new(&ws, &mem, None).expect("LainServer::new");

        let v = get_server_status(&server);
        assert!(v.get("pid").is_some(), "missing pid");
        assert!(v.get("transport").is_some(), "missing transport");
        assert!(v.get("port").is_some(), "missing port");
        assert!(v.get("started_at").is_some(), "missing started_at");
        assert!(v.get("last_sync_at").is_some(), "missing last_sync_at");
        assert!(v.get("last_error").is_some(), "missing last_error");
        assert!(v.get("repo_count").is_some(), "missing repo_count");
        assert!(v.get("workspace_count").is_some(), "missing workspace_count");

        // `pid` is the current process.
        assert_eq!(v["pid"].as_u64().unwrap(), std::process::id() as u64);
        // Single-workspace mode: no federation, so transport/port null.
        assert!(v["transport"].is_null());
        assert!(v["port"].is_null());
        // No federation → 0 of each.
        assert_eq!(v["repo_count"].as_u64().unwrap(), 0);
        assert_eq!(v["workspace_count"].as_u64().unwrap(), 0);
        // `started_at` and `last_sync_at` are populated (>= 0).
        assert!(v["started_at"].as_i64().unwrap() > 0);
        assert!(v["last_sync_at"].as_i64().unwrap() > 0);
        // No errors yet.
        assert!(v["last_error"].is_null());
    }

    #[test]
    fn get_server_status_reflects_record_last_error() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        git2::Repository::init(&ws).unwrap();
        let mem = tmp.path().join("graph.bin");
        let server = LainServer::new(&ws, &mem, None).unwrap();
        server.record_last_error("boom");
        let v = get_server_status(&server);
        assert_eq!(v["last_error"].as_str(), Some("boom"));
        // record_last_error also bumps last_sync_at.
        let v2 = get_server_status(&server);
        assert!(v2["last_sync_at"].as_i64().unwrap() >= v["last_sync_at"].as_i64().unwrap());
    }

    #[test]
    fn get_server_status_record_sync_clears_error() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        git2::Repository::init(&ws).unwrap();
        let mem = tmp.path().join("graph.bin");
        let server = LainServer::new(&ws, &mem, None).unwrap();
        server.record_last_error("boom");
        server.record_sync();
        let v = get_server_status(&server);
        assert!(v["last_error"].is_null());
    }
}

#[cfg(test)]
mod recent_projects_tests {
    use super::*;
    use crate::server::LainConfig;
    use std::path::PathBuf;

    /// Stub: build a minimal `LainConfig`. We never use this in the
    /// test (the tool reads from the on-disk recent_projects file, not
    /// from `LainServer`), but the import above keeps the harness
    /// honest about which `LainConfig` we're targeting.
    #[allow(dead_code)]
    fn _config_stub() -> LainConfig {
        LainConfig {
            workspace: PathBuf::from("/tmp"),
            memory_path: PathBuf::from("/tmp/graph.bin"),
        }
    }

    /// Build a `repos.yaml` + optional `workspaces.yaml` next to each
    /// other under `tmp`. Returns the path to `repos.yaml`.
    fn write_project(tmp: &std::path::Path, name: &str, repos: &[&str], workspaces: &[(&str, &[&str])]) -> PathBuf {
        let dir = tmp.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let repos_path = dir.join("repos.yaml");
        let mut yaml = String::from("repos:\n");
        for r in repos {
            yaml.push_str(&format!(
                "  - id: {r}\n    source: {{ type: workspace_dir, path: /srv/{r} }}\n"
            ));
        }
        std::fs::write(&repos_path, yaml).unwrap();
        if !workspaces.is_empty() {
            let mut ws_yaml = String::from("workspaces:\n");
            for (n, members) in workspaces {
                let members_list = members.join(", ");
                ws_yaml.push_str(&format!("  - name: {n}\n    members: [{members_list}]\n"));
            }
            std::fs::write(dir.join("workspaces.yaml"), ws_yaml).unwrap();
        }
        repos_path
    }

    /// Redirect the recent-projects file to a tempdir for the duration
    /// of the test. Uses `_in` variants so we don't race with other
    /// tests that may set `XDG_CONFIG_HOME`.
    fn with_temp_recent<F: FnOnce(&std::path::Path)>(f: F) {
        // Wrap F in a Box<dyn FnOnce> to satisfy `catch_unwind`'s
        // UnwindSafe bound; the actual function body never panics but
        // this matches the pattern used elsewhere in the codebase.
        let boxed: Box<dyn FnOnce(&std::path::Path)> = Box::new(f);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_path_buf();
        let mut f = boxed;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&path)));
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    /// RAII guard that points `XDG_CONFIG_HOME` at a tempdir for the
    /// duration of a test, restoring the previous value on drop. Used
    /// only by the production end-to-end test below; the other tests
    /// in this module prefer the `_in` helpers which don't touch the
    /// env var at all.
    struct XdgGuard {
        prev: Option<String>,
    }

    impl XdgGuard {
        fn new(dir: &std::path::Path) -> Self {
            let prev = std::env::var("XDG_CONFIG_HOME").ok();
            std::env::set_var("XDG_CONFIG_HOME", dir);
            Self { prev }
        }
    }

    impl Drop for XdgGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    #[test]
    fn list_recent_projects_returns_empty_when_file_missing() {
        with_temp_recent(|dir| {
            // Use the *_in variants so we don't touch XDG_CONFIG_HOME.
            let list = crate::config::recent_projects::list_in(dir).unwrap();
            assert!(list.is_empty());
        });
    }

    #[test]
    fn list_recent_projects_enriches_with_counts() {
        with_temp_recent(|dir| {
            let a = write_project(dir, "a", &["r1", "r2", "r3"], &[("team", &["r1", "r2"])]);
            let b = write_project(dir, "b", &["r4"], &[]);
            crate::config::recent_projects::record_in(dir, &a).unwrap();
            crate::config::recent_projects::record_in(dir, &b).unwrap();

            // `list_recent_projects()` reads from the production
            // config_dir; to test it end-to-end we'd need to stub the
            // config dir. Instead we test the per-entry count helper
            // directly and confirm it produces sane numbers.
            let (ws_count_a, repo_count_a) = counts_for_project(&a);
            assert_eq!(repo_count_a, 3);
            assert_eq!(ws_count_a, 1);
            let (ws_count_b, repo_count_b) = counts_for_project(&b);
            assert_eq!(repo_count_b, 1);
            assert_eq!(ws_count_b, 0);
        });
    }

    #[test]
    fn counts_for_project_zeros_when_file_missing() {
        let bogus = PathBuf::from("/nonexistent/repos.yaml");
        let (ws, repo) = counts_for_project(&bogus);
        assert_eq!(ws, 0);
        assert_eq!(repo, 0);
    }

    /// End-to-end test for the production `list_recent_projects()`
    /// function. Exercises the full chain — `record()` writes through
    /// `config_dir()`, which reads `XDG_CONFIG_HOME`, so we point
    /// `XDG_CONFIG_HOME` at a tempdir. Then we call the no-arg
    /// production function and serialize the result to JSON so the
    /// asserted field names match what the dashboard actually parses.
    /// Locks against `crate::state::TEST_LOCK` because
    /// `XDG_CONFIG_HOME` is process-global; the `state::tests` mod
    /// mutates the same env var under the same lock.
    #[test]
    fn list_recent_projects_production_end_to_end() {
        let _g = crate::state::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _xdg = XdgGuard::new(tmp.path());

        // Two synthetic projects with distinct repo / workspace counts.
        let a = write_project(
            tmp.path(),
            "a",
            &["r1", "r2", "r3"],
            &[("team", &["r1", "r2"])],
        );
        let b = write_project(tmp.path(), "b", &["r4"], &[]);

        // Use the production (no-_in) variants — they read
        // `config_dir()`, which now resolves to `<tmp>/lain/`.
        crate::config::recent_projects::record(&a).unwrap();
        crate::config::recent_projects::record(&b).unwrap();

        let entries = list_recent_projects().expect("list_recent_projects");
        assert_eq!(
            entries.len(),
            2,
            "expected 2 recent projects, got {:?}",
            entries.iter().map(|e| &e.path).collect::<Vec<_>>()
        );

        // Most-recent-first: `b` was recorded last.
        assert_eq!(entries[0].path, b);
        assert_eq!(entries[1].path, a);

        // Counts flowed through `counts_for_project` correctly.
        assert_eq!(entries[0].workspace_count, 0);
        assert_eq!(entries[0].repo_count, 1);
        assert_eq!(entries[1].workspace_count, 1);
        assert_eq!(entries[1].repo_count, 3);

        // Timestamps populated (record() stamps `now_unix`).
        assert!(entries[0].last_used > 0);
        assert!(entries[1].last_used > 0);
        assert!(entries[0].last_used >= entries[1].last_used);

        // Serialize to JSON and verify the field names the dashboard
        // parses. Default serde rename keeps struct field names, so
        // any future `#[serde(rename = ...)]` or accidental rename
        // would surface here as a missing-key assertion failure.
        let json = serde_json::to_value(&entries).expect("serialize entries");
        let arr = json.as_array().expect("entries serialize as JSON array");
        assert_eq!(arr.len(), 2);
        for (i, item) in arr.iter().enumerate() {
            let obj = item.as_object().expect("each entry is a JSON object");
            for field in ["path", "last_used", "workspace_count", "repo_count"] {
                assert!(
                    obj.contains_key(field),
                    "entry[{i}] missing required JSON field `{field}`; got keys {:?}",
                    obj.keys().collect::<Vec<_>>()
                );
            }
        }
        // Spot-check concrete values in the JSON shape.
        assert_eq!(arr[0]["path"], serde_json::json!(b.to_string_lossy()));
        assert_eq!(arr[0]["workspace_count"], serde_json::json!(0));
        assert_eq!(arr[0]["repo_count"], serde_json::json!(1));
        assert_eq!(arr[1]["path"], serde_json::json!(a.to_string_lossy()));
        assert_eq!(arr[1]["workspace_count"], serde_json::json!(1));
        assert_eq!(arr[1]["repo_count"], serde_json::json!(3));
        // `last_used` is an integer in both entries.
        assert!(arr[0]["last_used"].as_i64().unwrap() > 0);
        assert!(arr[1]["last_used"].as_i64().unwrap() > 0);
    }
}
