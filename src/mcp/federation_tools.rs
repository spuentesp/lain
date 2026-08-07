//! Federation-mode MCP tools.
//!
//! Exposes four read-only tools for inspecting the live state of a
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
//!
//! All tools are gated on the MCP server having been constructed with a
//! `FederatedIndex` (see `LainMcpServer::with_federation`). When the server
//! runs in single-workspace mode these tools are not registered and the
//! existing tool surface is unchanged.

use crate::error::LainError;
use crate::federation::federated_index::FederatedIndex;
use crate::federation::repo_id::{GlobalId, RepoId};
use serde::{Deserialize, Serialize};

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
}
