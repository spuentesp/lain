//! Federation — index, workspaces, transport, repos config.
//!
//! Extracted from `LainServer` in PR 3.7a. `LainServer` will hold an
//! `Arc<FederationHandle>` in PR 3.7b and forward the 12 federation
//! methods through.

use crate::server::federation::config::RepoConfig;
use crate::server::federation::federated_index::FederatedIndex;
use crate::server::federation::repo_id::RepoId;
use crate::server::federation::workspace::WorkspacesFile;
use crate::server::ingest::config::Transport;
use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Federation index, workspaces lock, transport, port, and the
/// `repos.yaml` path the server was launched with. `None` on every
/// field for single-workspace servers constructed via `LainServer::new`.
pub struct FederationHandle {
    pub(crate) federation: Option<Arc<FederatedIndex>>,
    pub(crate) federation_workspaces: Option<Arc<RwLock<WorkspacesFile>>>,
    pub(crate) federation_transport: Option<Transport>,
    pub(crate) federation_port: Option<u16>,
    pub(crate) repos_yaml: Option<PathBuf>,
}

impl FederationHandle {
    pub fn new(
        federation: Option<Arc<FederatedIndex>>,
        federation_workspaces: Option<Arc<RwLock<WorkspacesFile>>>,
        federation_transport: Option<Transport>,
        federation_port: Option<u16>,
        repos_yaml: Option<PathBuf>,
    ) -> Self {
        Self {
            federation,
            federation_workspaces,
            federation_transport,
            federation_port,
            repos_yaml,
        }
    }

    /// Federation accessor. Returns `None` for single-workspace servers.
    pub fn federation(&self) -> Option<&Arc<FederatedIndex>> {
        self.federation.as_ref()
    }

    /// The shared workspaces lock for federation-mode servers with workspaces.
    pub fn federation_workspaces(&self) -> Option<&Arc<RwLock<WorkspacesFile>>> {
        self.federation_workspaces.as_ref()
    }

    /// The federation transport (Http or Stdio).
    pub fn federation_transport(&self) -> Option<Transport> {
        self.federation_transport
    }

    /// The federation HTTP port (None for stdio or single-workspace).
    pub fn federation_port(&self) -> Option<u16> {
        self.federation_port
    }

    /// Transport for the active MCP server. `None` for single-workspace
    /// servers (not federation-mode); some for federation-mode.
    pub fn transport(&self) -> Option<Transport> {
        self.federation_transport
    }

    /// TCP port for HTTP federation-mode servers; `None` for stdio or
    /// single-workspace servers.
    pub fn port(&self) -> Option<u16> {
        self.federation_port
    }

    /// Path to the `repos.yaml` this server was launched with, if any.
    /// `None` for single-workspace servers.
    pub fn repos_yaml(&self) -> Option<&Path> {
        self.repos_yaml.as_deref()
    }

    /// Number of repos in the live federation, or 0 for single-workspace
    /// servers.
    pub fn repo_count(&self) -> usize {
        self.federation
            .as_ref()
            .map(|f| f.list_repos().len())
            .unwrap_or(0)
    }

    /// Number of workspaces in the loaded `workspaces.yaml`, or 0 when
    /// none was supplied. Reads through the `Arc<RwLock<...>>` slot so a
    /// hot-reload that swaps `set_workspace` is reflected on the next
    /// call.
    pub fn workspace_count(&self) -> usize {
        self.federation_workspaces
            .as_ref()
            .map(|w| w.read().workspaces.len())
            .unwrap_or(0)
    }

    /// Sorted list of repo ids in this federation. Returns an empty
    /// slice for single-workspace servers that don't have a
    /// `FederatedIndex`. Used by the annotation tools to enumerate
    /// the per-repo stores on every cross-repo query.
    pub fn federation_repos(&self) -> Vec<RepoId> {
        match self.federation.as_ref() {
            Some(fed) => {
                let mut ids: Vec<_> = fed.list_repos().into_iter().map(|(id, _)| id).collect();
                ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
                ids
            }
            None => Vec::new(),
        }
    }

    /// Add a repo to the live federation, then project its nodes/edges
    /// into the global backend. No-op if `self.federation` is `None`
    /// (single-workspace mode).
    ///
    /// `repo` is the `RepoConfig` entry as written to `repos.yaml`. The
    /// data directory for the per-repo indexer is computed from the
    /// server's stored `repos.yaml` path (or, when none is set, from
    /// the default `./.lain/federation`). The `data_dir` resolution is
    /// performed by the caller (`run_rebuild`) and passed in via the
    /// `data_dir` argument; here we just call through to the
    /// federation.
    pub async fn add_repo(
        &self,
        repo: &RepoConfig,
        data_dir: &Path,
    ) -> Result<(), crate::server::error::LainError> {
        let fed = self.federation.as_ref().ok_or_else(|| {
            crate::server::error::LainError::Other(
                "FederationHandle::add_repo called on a non-federation server".into(),
            )
        })?;
        let config = crate::server::federation::config::FederationConfig {
            data_dir: data_dir.to_path_buf(),
            ..Default::default()
        };
        let source = config.build_source_for(repo).map_err(|e| {
            crate::server::error::LainError::Config(format!("build_source_for({}): {e}", repo.id))
        })?;
        // `WorkspaceDirSource::fetch` is a no-op; `LocalCloneSource` and
        // `ShallowCloneSource` actually clone. Hot-reload only sees
        // already-on-disk sources (`workspace_dir`), but we still call
        // `fetch` so adding a freshly-written `local_clone` entry also
        // works end-to-end.
        source.fetch().await?;
        let repo_id = source.id().clone();
        fed.add_repo(source, data_dir).await?;
        fed.project_repo(&repo_id).await?;
        // Index it, as startup does for every repo: adding only registered
        // an empty graph that stayed `indexing` until the next restart.
        // Then link calls between it and the others, and watch it.
        let fed = Arc::clone(fed);
        let id = repo_id.clone();
        tokio::spawn(async move {
            let Some(repo) = fed.get_repo(&id) else {
                return;
            };
            if let Err(e) = repo.index().await {
                tracing::warn!("indexing hot-added repo '{id}' failed: {e}");
                repo.set_health(crate::federation::health::RepoHealth::Degraded);
                return;
            }
            for (other, _) in fed.list_repos() {
                if let Some(r) = fed.get_repo(&other) {
                    if let Err(e) = r.relink_cross_repo().await {
                        tracing::warn!("cross-repo link for '{other}' failed: {e}");
                    }
                }
                if let Err(e) = fed.project_repo(&other).await {
                    tracing::warn!("project_repo for '{other}' failed: {e}");
                }
            }
            if let Err(e) = repo.start_watcher().await {
                tracing::warn!("could not watch hot-added repo '{id}': {e}");
            }
        });
        // `record_sync` lives on `RefreshState` in PR 3.7b; this handle
        // signals "sync happened" via a callback so the LainServer
        // can route it. In this PR we just log and let the caller
        // invoke `record_sync` on its own `RefreshState`.
        tracing::debug!("federation add_repo succeeded: {repo_id}");
        Ok(())
    }

    /// Remove a repo from the live federation. No-op if `self.federation`
    /// is `None` (single-workspace mode).
    pub fn remove_repo(&self, repo_id: &str) -> Result<(), crate::server::error::LainError> {
        let fed = self.federation.as_ref().ok_or_else(|| {
            crate::server::error::LainError::Other(
                "FederationHandle::remove_repo called on a non-federation server".into(),
            )
        })?;
        let rid = RepoId::new(repo_id).map_err(|e| {
            crate::server::error::LainError::Config(format!("invalid repo id '{repo_id}': {e}"))
        })?;
        fed.remove_repo(&rid)?;
        Ok(())
    }

    /// Replace the workspace file the server exposes through MCP. Called
    /// by `run_rebuild` after re-reading `workspaces.yaml`. Writes
    /// through the SAME `Arc<RwLock<WorkspacesFile>>` the
    /// `LainMcpServer` constructed by `serve` is holding, so the very
    /// next dispatch of `list_workspaces` / `get_workspace` /
    /// `get_workspace_graph` observes the new contents without a
    /// server restart.
    ///
    /// No-op for single-workspace servers (no workspaces file).
    pub fn set_workspace(&self, workspaces: Arc<WorkspacesFile>) {
        if let Some(slot) = &self.federation_workspaces {
            *slot.write() = (*workspaces).clone();
        }
    }

    /// Cheap snapshot of the loaded workspaces file. Used by
    /// `run_rebuild` to seed a `set_workspace` no-op diff when nothing
    /// changed and to test the slot swap. Clones the inner
    /// `WorkspacesFile`, so callers should avoid calling this in tight
    /// loops; reads are cheap on the lock side.
    pub fn workspaces_snapshot(&self) -> Option<Arc<WorkspacesFile>> {
        self.federation_workspaces
            .as_ref()
            .map(|slot| Arc::new(slot.read().clone()))
    }

    /// Shared handle to the live workspaces lock, or `None` for
    /// single-workspace servers. This is the SAME
    /// `Arc<RwLock<WorkspacesFile>>` the `LainMcpServer` constructed
    /// by `serve()` holds inside its `workspaces` field, so callers
    /// can verify the hot-reload fix end-to-end: a `set_workspace`
    /// followed by a `handle.read()` on any thread (including from a
    /// JSON-RPC dispatch) sees the new value without any
    /// synchronization beyond the rwlock's own barriers.
    pub fn workspaces_handle(&self) -> Option<Arc<RwLock<WorkspacesFile>>> {
        self.federation_workspaces.as_ref().map(Arc::clone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_workspace_construction_leaves_everything_none_or_zero() {
        let handle = FederationHandle::new(None, None, None, None, None);
        assert!(handle.federation().is_none());
        assert_eq!(handle.transport(), None);
        assert_eq!(handle.port(), None);
        assert_eq!(handle.repos_yaml(), None);
        assert_eq!(handle.repo_count(), 0);
        assert_eq!(handle.workspace_count(), 0);
        assert!(handle.federation_repos().is_empty());
        assert!(handle.workspaces_handle().is_none());
    }

    #[test]
    fn add_repo_errors_on_single_workspace() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let handle = FederationHandle::new(None, None, None, None, None);
        let tmp = tempfile::tempdir().unwrap();
        let repo = RepoConfig {
            id: "test".to_string(),
            source: crate::server::federation::config::SourceConfig::WorkspaceDir {
                path: tmp.path().to_path_buf(),
            },
        };
        let res = rt.block_on(handle.add_repo(&repo, tmp.path()));
        assert!(res.is_err());
    }

    #[test]
    fn remove_repo_errors_on_single_workspace() {
        let handle = FederationHandle::new(None, None, None, None, None);
        let res = handle.remove_repo("any");
        assert!(res.is_err());
    }
}
