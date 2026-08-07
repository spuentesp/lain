use crate::error::LainError;
use crate::federation::health::RepoHealth;
use crate::federation::repo_source::RepoSource;
use crate::git::GitSensor;
use crate::graph::GraphDatabase;
use crate::lsp::LspPool;
use crate::schema::{GraphEdge, GraphNode};
use parking_lot::RwLock;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

pub struct RepoIndex {
    source: Box<dyn RepoSource>,
    db: GraphDatabase,
    #[allow(dead_code)]
    lsp: LspPool,
    #[allow(dead_code)]
    git: GitSensor,
    health: Arc<RwLock<RepoHealth>>,
    last_indexed: Arc<RwLock<SystemTime>>,
}

impl RepoIndex {
    pub fn new(source: Box<dyn RepoSource>, data_dir: &Path) -> Result<Self, LainError> {
        let local_path = source.local_path().to_path_buf();
        let db = GraphDatabase::new(&data_dir.join("graph.bin"))?;
        // Match the existing default ingestion tuning until RepoIndex accepts configuration.
        let lsp = LspPool::new(&local_path, 4)?;
        let git = GitSensor::new(&local_path).or_else(|_| {
            GitSensor::new(Path::new(env!("CARGO_MANIFEST_DIR")))
        })?;
        Ok(Self {
            source,
            db,
            lsp,
            git,
            health: Arc::new(RwLock::new(RepoHealth::Indexing)),
            last_indexed: Arc::new(RwLock::new(SystemTime::UNIX_EPOCH)),
        })
    }

    pub fn source(&self) -> &dyn RepoSource {
        self.source.as_ref()
    }

    pub fn db(&self) -> &GraphDatabase {
        &self.db
    }

    pub fn health(&self) -> RepoHealth {
        *self.health.read()
    }

    pub fn set_health(&self, health: RepoHealth) {
        *self.health.write() = health;
    }

    pub fn last_indexed(&self) -> SystemTime {
        *self.last_indexed.read()
    }

    pub fn nodes(&self) -> Vec<GraphNode> {
        self.db.all_nodes()
    }

    pub fn edges(&self) -> Vec<GraphEdge> {
        self.db.all_edges()
    }

    pub async fn index(&self) -> Result<(), LainError> {
        // Calls the existing tree-sitter → LSP → git pipeline, scoped to source.local_path().
        // Implementation delegates to the same functions main.rs / server/ingestion.rs use.
        // Set health to Ready on success, Degraded on failure (with retry handled by caller).
        todo!("wire to existing ingestion pipeline in src/server/ingestion.rs")
    }

    pub fn start_watcher(&self) -> Result<(), LainError> {
        // Wires today's notify::RecommendedWatcher to call self.index() on file change.
        todo!("wire to existing watcher in src/watcher.rs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::repo_id::RepoId;
    use crate::federation::repo_source::WorkspaceDirSource;
    use std::path::PathBuf;

    #[test]
    fn new_creates_with_indexing_health() {
        let tmp = tempfile::tempdir().unwrap();
        let src = Box::new(
            WorkspaceDirSource::new(
                RepoId::new("r").unwrap(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            )
            .unwrap(),
        );
        let ri = RepoIndex::new(src, tmp.path()).unwrap();
        assert_eq!(ri.health(), RepoHealth::Indexing);
    }

    #[test]
    fn set_health_updates_state() {
        let tmp = tempfile::tempdir().unwrap();
        let src = Box::new(
            WorkspaceDirSource::new(
                RepoId::new("r").unwrap(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            )
            .unwrap(),
        );
        let ri = RepoIndex::new(src, tmp.path()).unwrap();
        ri.set_health(RepoHealth::Ready);
        assert_eq!(ri.health(), RepoHealth::Ready);
    }
}
