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
use crate::server::overlay::VolatileOverlay;
use dashmap::DashMap;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

pub struct FederatedIndex {
    repos: RwLock<HashMap<RepoId, Arc<RepoIndex>>>,
    backend: Arc<dyn GraphBackend>,
    symbol_to_repos: DashMap<String, Vec<RepoId>>,
    /// Shared `VolatileOverlay` installed into each `RepoIndex` on
    /// construction via [`Self::install_overlay`]. The federation's
    /// `VolatileOverlay` is what the server returns as the "overlay
    /// freshness" — without this wiring every freshly-indexed server
    /// would show "stale" forever because the indexer doesn't insert
    /// through the overlay. `None` until [`Self::install_overlay`]
    /// is called (the test harness never calls it).
    federation_overlay: RwLock<Option<Arc<VolatileOverlay>>>,
    /// Fraction of repos that must reach `Ready` before the federation
    /// reports itself healthy. From `repos.yaml`'s `ready_threshold`.
    ///
    /// `docs/REPOS_YAML.md` documents this as "Fraction of repos that
    /// must reach `Ready` health before the federation reports
    /// `healthy`" — but nothing read the setting and `FederationHealth`
    /// had no `healthy` field at all, so the documented behaviour did
    /// not exist in any form.
    ready_threshold: RwLock<f32>,
}

/// Collapse a per-definition repo list to the distinct repos in it,
/// preserving order.
///
/// `resolve_symbol` answers "which repo owns this name?", so the same
/// repo listed once per definition is one answer, not several. Without
/// this, an ordinary name — `new`, `default`, a helper repeated across
/// modules — made `resolve_symbol` fail with
/// `AmbiguousSymbol(["lain", "lain"])`: a request to disambiguate
/// between one repo and itself, naming a `repo_id` parameter the tool
/// schema does not expose. Unactionable by construction.
pub(crate) fn distinct_repos(entries: &[RepoId]) -> Vec<RepoId> {
    let mut out: Vec<RepoId> = Vec::with_capacity(entries.len());
    for e in entries {
        if !out.contains(e) {
            out.push(e.clone());
        }
    }
    out
}

impl FederatedIndex {
    pub fn new(backend: Arc<dyn GraphBackend>) -> Self {
        Self {
            repos: RwLock::new(HashMap::new()),
            backend,
            symbol_to_repos: DashMap::new(),
            federation_overlay: RwLock::new(None),
            ready_threshold: RwLock::new(crate::federation::config::DEFAULT_READY_THRESHOLD),
        }
    }

    /// Install the readiness threshold from `repos.yaml`.
    pub fn set_ready_threshold(&self, threshold: f32) {
        *self.ready_threshold.write() = threshold.clamp(0.0, 1.0);
    }

    /// Fraction of repos that must be `Ready` for the federation to
    /// report healthy.
    pub fn ready_threshold(&self) -> f32 {
        *self.ready_threshold.read()
    }

    /// Wire the federation's shared `VolatileOverlay` into every
    /// existing and future `RepoIndex`. Idempotent — calling twice with
    /// different overlays swaps the active one and updates all live
    /// repos. Called once by `build_federation_server` after the
    /// `VolatileOverlay` is constructed.
    pub fn install_overlay(&self, overlay: Arc<VolatileOverlay>) {
        *self.federation_overlay.write() = Some(overlay.clone());
        for repo in self.repos.read().values() {
            repo.set_overlay(overlay.clone());
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
        // If the federation already has an overlay wired in (the
        // production constructor installs it before the first `add_repo`),
        // share the same Arc so a successful index() updates the
        // server-wide freshness banner.
        if let Some(overlay) = self.federation_overlay.read().clone() {
            index.set_overlay(overlay);
        }
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

    /// Local checkout paths of every registered repo. The attribution
    /// watcher monitors exactly these roots — watching `repos.yaml`'s
    /// parent dir instead swept in unrelated files (server logs,
    /// scratch files) and auto-claimed them under the single-agent
    /// heuristic.
    pub fn repo_paths(&self) -> Vec<std::path::PathBuf> {
        self.repos
            .read()
            .values()
            .map(|idx| idx.source().local_path().to_path_buf())
            .collect()
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
        let mut local_to_global: std::collections::HashMap<String, String> =
            std::collections::HashMap::with_capacity(nodes.len());
        let mut batch_nodes: Vec<crate::schema::GraphNode> = Vec::with_capacity(nodes.len());
        for n in &nodes {
            let gid = GlobalId::new(id, n.node_type.clone(), &n.path, &n.name);
            let mut rewritten = n.clone();
            rewritten.id = gid.as_str().to_string();
            live.insert(gid.as_str().to_string());
            batch_nodes.push(rewritten);
            local_to_global.insert(n.id.clone(), gid.as_str().to_string());
        }
        // Batch upsert: one disk save at the end instead of ~N syncs.
        // The per-node path saved on every upsert and wedged the
        // loader for seconds-to-minutes on large repos (3k+ nodes).
        self.backend.upsert_nodes_batch(&batch_nodes)?;
        tracing::info!(
            "[federation] {:?}: projected {} nodes",
            id.as_str(),
            batch_nodes.len()
        );

        // Project intra-repo edges (Calls / Contains / Uses / ...) with
        // their endpoint ids rewritten to global ids. The backend
        // upserts are idempotent on edge identity (source+target+
        // edge_type), so re-running `project_repo` is safe. Edges with
        // either endpoint missing from the local-to-global map (e.g.
        // scanner-introduced virtual edges) are skipped — they'll show
        // up next time the scanner emits them with stable ids.
        //
        // BATCH: ~10k edges is normal for a large repo. The per-edge
        // upsert saves the backend graph on every call, which would
        // take minutes. Batch all the writes and save once.
        let db = repo.db();
        let mut batch: Vec<crate::schema::GraphEdge> = Vec::new();
        for edge in &db.all_edges() {
            let Some(src) = local_to_global.get(&edge.source_id) else {
                continue;
            };
            let Some(tgt) = local_to_global.get(&edge.target_id) else {
                continue;
            };
            batch.push(crate::schema::GraphEdge {
                edge_type: edge.edge_type.clone(),
                source_id: src.clone(),
                target_id: tgt.clone(),
                weight: edge.weight,
            });
        }
        self.backend.upsert_edges_batch(&batch)?;
        tracing::info!(
            "[federation] {:?}: projected {} intra-repo edges",
            id.as_str(),
            batch.len()
        );

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
            // Ambiguity is about *repos*, not definitions. Both paths go
            // through `distinct_repos` so a name defined several times
            // in one repo can never be reported as a cross-repo clash.
            let distinct = distinct_repos(entries.value());
            if !distinct.is_empty() {
                return match distinct.len() {
                    1 => Ok(distinct.into_iter().next().unwrap()),
                    _ => Err(LainError::AmbiguousSymbol(distinct)),
                };
            }
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
            // The graph indexes committed code. Saying only "not found"
            // reads as "this symbol does not exist", which is how an
            // agent concludes a function it can see in the working tree
            // was deleted.
            0 => Err(LainError::NotFound(format!(
                "symbol {name} not found in any indexed repo — the federation graph \
                 indexes committed code, so a symbol added since the last indexed \
                 commit will not appear until it is committed and re-indexed \
                 (check `get_health` for how far behind HEAD the graph is)"
            ))),
            1 => Ok(hits.into_iter().next().unwrap()),
            _ => Err(LainError::AmbiguousSymbol(distinct_repos(&hits))),
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
                    let entry = tmp.entry(node.name.clone()).or_default();
                    // One entry per *repo*, not per node. A name defined
                    // more than once inside a single repo — which is
                    // ordinary: `new`, `default`, a helper repeated across
                    // modules — used to push that repo's id once per
                    // definition. `resolve_symbol`'s fast path then saw
                    // `len() != 1` and returned
                    // `AmbiguousSymbol(["lain", "lain"])`: a request to
                    // disambiguate between one repo and itself, with a
                    // `repo_id` parameter the tool schema doesn't even
                    // expose. Only the slow fallback deduped, so the bug
                    // appeared exactly when the index was warm.
                    if !entry.contains(repo_id) {
                        entry.push(repo_id.clone());
                    }
                }
            }
        }
        for (k, v) in tmp {
            self.symbol_to_repos.insert(k, v);
        }
    }
}
