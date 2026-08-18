//! Federation orchestrator. Owns N per-repo `RepoIndex` workers, projects their
//! nodes/edges into a global petgraph via `GraphBackend`, and provides
//! cross-repo symbol resolution.
//!
//! The global ID format is `repo_id:Kind:path:name` (see `GlobalId::new`), and
//! every per-repo node is re-keyed to that format before being upserted into the
//! backend. Cross-repo edges (`CrossRepoSameSymbol`) are added by running
//! `find_cross_repo_matches` against signatures.
//!
//! The `symbol_to_repos` index is built from each repo's `nodes()` on every
//! `add_repo` and `project_repo`. That is O(repos * nodes) per rebuild; the
//! plan flags this as acceptable for the MVP and notes a separate name index
//! would be the production implementation.
use crate::error::LainError;
use crate::federation::graph_backend::GraphBackend;
use crate::federation::health::RepoHealth;
use crate::federation::matching::find_cross_repo_matches;
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::federation::repo_index::RepoIndex;
use crate::federation::repo_source::RepoSource;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use dashmap::DashMap;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

pub struct FederatedIndex {
    repos: RwLock<HashMap<RepoId, Arc<RepoIndex>>>,
    backend: Arc<dyn GraphBackend>,
    symbol_to_repos: DashMap<String, Vec<RepoId>>,
}

impl FederatedIndex {
    pub fn new(backend: Arc<dyn GraphBackend>) -> Self {
        Self {
            repos: RwLock::new(HashMap::new()),
            backend,
            symbol_to_repos: DashMap::new(),
        }
    }

    pub async fn add_repo(
        &self,
        source: Box<dyn RepoSource>,
        data_dir: &Path,
    ) -> Result<(), LainError> {
        let id = source.id().clone();
        // Per-repo state goes in a per-repo subdir under data_dir so
        // workspace restarts that load a different member set don't
        // collide on a shared file (the pre-fix behavior clobbered state
        // across repos and broke any workspace switch to a new repo_id).
        let per_repo_dir = data_dir.join("repos").join(id.as_str());
        std::fs::create_dir_all(&per_repo_dir).map_err(|e| LainError::Io(e.to_string()))?;
        let index = Arc::new(RepoIndex::new(source, &per_repo_dir)?);
        self.repos.write().insert(id, index);
        self.rebuild_symbol_index();
        Ok(())
    }

    pub fn remove_repo(&self, id: &RepoId) -> Result<(), LainError> {
        self.repos.write().remove(id);
        self.rebuild_symbol_index();
        Ok(())
    }

    pub fn get_repo(&self, id: &RepoId) -> Option<Arc<RepoIndex>> {
        self.repos.read().get(id).cloned()
    }

    pub fn list_repos(&self) -> Vec<(RepoId, RepoHealth)> {
        let mut out: Vec<(RepoId, RepoHealth)> = self
            .repos
            .read()
            .iter()
            .map(|(id, idx)| (id.clone(), idx.health()))
            .collect();
        out.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        out
    }

    pub fn global_id(&self, repo: &RepoId, kind: NodeType, path: &str, name: &str) -> GlobalId {
        GlobalId::new(repo, kind, path, name)
    }

    pub fn backend(&self) -> Arc<dyn GraphBackend> {
        self.backend.clone()
    }

    pub async fn project_repo(&self, id: &RepoId) -> Result<(), LainError> {
        let repo = self
            .get_repo(id)
            .ok_or_else(|| LainError::NotFound(format!("repo {id}")))?;
        let nodes = repo.nodes();

        // Re-key every node to its global id and upsert into the backend.
        let mut live: std::collections::HashSet<String> =
            std::collections::HashSet::with_capacity(nodes.len());
        for n in &nodes {
            let gid = GlobalId::new(id, n.node_type.clone(), &n.path, &n.name);
            let mut rewritten = n.clone();
            rewritten.id = gid.as_str().to_string();
            live.insert(gid.as_str().to_string());
            self.backend.upsert_node(rewritten)?;
        }

        // Retract what this repo no longer has. Projection was upsert-only, so
        // the federated view accumulated every symbol a repo ever contained: a
        // deleted function kept answering `search_org` long after the per-repo
        // graph had dropped it. Scoped by the repo's global-id prefix, which is
        // unambiguous because `RepoId` forbids `:`.
        let prefix = format!("{}:", id.as_str());
        let stale: Vec<String> = self
            .backend
            .list_nodes()?
            .into_iter()
            .map(|n| n.id)
            .filter(|gid| gid.starts_with(&prefix) && !live.contains(gid))
            .collect();
        if !stale.is_empty() {
            let removed = self.backend.remove_nodes(&stale)?;
            tracing::info!(
                "federation: retracted {removed} stale node(s) for repo '{}'",
                id.as_str()
            );
        }

        // Cross-repo matching: gather every other repo's nodes once, then for
        // each of this repo's nodes find similarity candidates above threshold.
        let other_nodes: Vec<GraphNode> = self
            .repos
            .read()
            .iter()
            .filter(|(rid, _)| *rid != id)
            .flat_map(|(_, idx)| idx.nodes())
            .collect();
        for new_node in &nodes {
            let matches = find_cross_repo_matches(new_node, &other_nodes, 5, 0.5);
            for (target_gid, sim) in matches {
                self.backend.upsert_edge(GraphEdge {
                    edge_type: EdgeType::CrossRepoSameSymbol,
                    source_id: GlobalId::new(id, new_node.node_type.clone(), &new_node.path, &new_node.name)
                        .as_str()
                        .to_string(),
                    target_id: target_gid,
                    weight: Some(sim),
                })?;
            }
        }

        self.rebuild_symbol_index();
        Ok(())
    }

    pub fn resolve_symbol(&self, name: &str) -> Result<RepoId, LainError> {
        // Fast path: the in-memory name index built by `rebuild_symbol_index`.
        if let Some(entries) = self.symbol_to_repos.get(name) {
            return match entries.len() {
                1 => Ok(entries[0].clone()),
                _ => Err(LainError::AmbiguousSymbol(entries.clone())),
            };
        }
        // Fallback: scan the backend. This keeps `resolve_symbol` correct even
        // when nodes were inserted directly into the backend (e.g. tests, or
        // other writers that bypass `add_repo` / `project_repo`).
        let mut hits: Vec<RepoId> = self
            .backend
            .find_nodes_by_name(name)?
            .into_iter()
            .filter_map(|n| match crate::federation::repo_id::GlobalId::parse(&n.id) {
                Ok(gid) => RepoId::new(gid.repo_id()).ok(),
                Err(_) => None,
            })
            .collect();
        hits.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        hits.dedup();
        match hits.len() {
            0 => Err(LainError::NotFound(format!(
                "symbol {name} not found in any repo"
            ))),
            1 => Ok(hits.into_iter().next().unwrap()),
            _ => Err(LainError::AmbiguousSymbol(hits)),
        }
    }

    fn rebuild_symbol_index(&self) {
        self.symbol_to_repos.clear();
        let mut tmp: HashMap<String, Vec<RepoId>> = HashMap::new();
        // Snapshot the repo ids we have, then release the read lock before
        // collecting nodes from each repo (avoids holding the lock across
        // potentially-slow node reads).
        let repo_ids: Vec<RepoId> = self.repos.read().iter().map(|(id, _)| id.clone()).collect();
        for repo_id in &repo_ids {
            if let Some(idx) = self.get_repo(repo_id) {
                for node in idx.nodes() {
                    tmp.entry(node.name.clone()).or_default().push(repo_id.clone());
                }
            }
        }
        for (k, v) in tmp {
            self.symbol_to_repos.insert(k, v);
        }
    }
}
