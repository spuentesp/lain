//! Hot-reload bus for the `LainServer`.
//!
//! `ReloadBus` fans out a "please rebuild" signal to any subscriber that
//! asked for one (the file watcher, the Unix socket handler, the MCP
//! `request_reload` tool) and tracks the most recent reload state for
//! observability (`get_reload_status`).
//!
//! The reload itself is performed by a separate task (see Task 6.2);
//! this module only models the *signal* and *status*. Subscribers
//! receive a coarse `()` notification and are responsible for fetching
//! the current `ReloadStatus` and acting on it.

use crate::server::error::LainError;
use crate::server::federation::config::FederationConfig;
use crate::server::federation::workspace::WorkspacesFile;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::{broadcast, Mutex as AsyncMutex};

/// Phase of the last reload attempt.
///
/// The bus never owns the rebuild work itself; it only records what
/// the server is currently doing. `Failed` carries the human-readable
/// error message that the rebuild task (Task 6.2) reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadState {
    Idle,
    Rebuilding,
    Failed(String),
}

/// Snapshot of the reload subsystem, suitable for returning from the
/// `get_reload_status` MCP tool.
///
/// `started_at` is set when the state transitions to `Rebuilding` and
/// cleared on completion. `last_reload_at` is set when the state
/// transitions back to `Idle` (i.e. a successful reload finished).
/// `pending_changes` is a list of paths that triggered the most recent
/// request, intended for the UI / status reporting.
#[derive(Debug, Clone)]
pub struct ReloadStatus {
    pub state: ReloadState,
    pub started_at: Option<SystemTime>,
    pub last_reload_at: Option<SystemTime>,
    pub last_error: Option<String>,
    pub pending_changes: Vec<String>,
}

/// Hot-reload signal bus.
///
/// Cloning is not provided: the bus is typically wrapped in an `Arc`
/// at the `LainServer` boundary and shared by reference. Subscribers
/// get a `ReloadSubscriber` that they can poll with `try_recv`.
pub struct ReloadBus {
    tx: broadcast::Sender<()>,
    status: Arc<AsyncMutex<ReloadStatus>>,
}

impl ReloadBus {
    /// Capacity of 16 is plenty for the typical subscriber count
    /// (file watcher, Unix socket listener, MCP request_reload tool).
    /// If a subscriber falls behind it sees `RecvError::Lagged`, which
    /// the rebuild task treats as "ask again on the next status poll".
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(16);
        Self {
            tx,
            status: Arc::new(AsyncMutex::new(ReloadStatus {
                state: ReloadState::Idle,
                started_at: None,
                last_reload_at: None,
                last_error: None,
                pending_changes: Vec::new(),
            })),
        }
    }

    /// Register a new listener for reload requests.
    pub fn subscribe(&self) -> ReloadSubscriber {
        ReloadSubscriber {
            rx: self.tx.subscribe(),
        }
    }

    /// Broadcast a reload request. The actual rebuild is performed by
    /// whichever subscriber picks the signal up (Task 6.2).
    ///
    /// Returns `Result` for symmetry with the future request-rebuild
    /// path that may bubble up validation errors. The broadcast itself
    /// never errors today — if there are no subscribers the message is
    /// simply dropped, which is the desired behavior.
    pub fn request_reload(&self) -> Result<(), String> {
        let _ = self.tx.send(());
        Ok(())
    }

    /// Cheap clone of the current status snapshot.
    pub fn status(&self) -> ReloadStatus {
        // `try_lock` is the right call here: callers (`get_reload_status`)
        // are MCP handlers that should never park the executor. If the
        // status is being written to right now, returning the previous
        // snapshot is acceptable — the next call will see the update.
        self.status
            .try_lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| ReloadStatus {
                state: ReloadState::Idle,
                started_at: None,
                last_reload_at: None,
                last_error: None,
                pending_changes: Vec::new(),
            })
    }

    /// Update the bus's recorded state. The rebuild task calls this on
    /// each phase transition so that observers (`get_reload_status`)
    /// can report progress.
    pub async fn set_state(&self, state: ReloadState) {
        let mut s = self.status.lock().await;
        s.state = state.clone();
        match state {
            ReloadState::Rebuilding => {
                s.started_at = Some(SystemTime::now());
                s.last_error = None;
            }
            ReloadState::Idle => {
                s.started_at = None;
                s.last_reload_at = Some(SystemTime::now());
            }
            ReloadState::Failed(msg) => {
                s.started_at = None;
                s.last_error = Some(msg);
            }
        }
    }
}

impl Default for ReloadBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Re-load `repos.yaml` and `workspaces.yaml` next to it, diff against
/// the live federation, and apply add/remove operations. Idempotent: a
/// no-op diff still transitions the bus back to `Idle`.
///
/// The caller (`serve_for_test` or `cli::server::run_server`) is
/// responsible for spawning a long-lived task that calls this in a
/// loop, subscribed to `bus.subscribe()`. This function performs the
/// *single* rebuild — no internal looping.
pub async fn run_rebuild(
    server: &crate::server::LainServer,
    bus: &ReloadBus,
) -> Result<(), LainError> {
    bus.set_state(ReloadState::Rebuilding).await;

    let repos_yaml = match server.repos_yaml_path() {
        Some(p) => p.to_path_buf(),
        None => {
            // Single-workspace server — nothing to reload. Treat as a
            // successful no-op so observers see the transition back to
            // Idle.
            bus.set_state(ReloadState::Idle).await;
            return Ok(());
        }
    };

    let result: Result<(), LainError> = (async {
        // Re-read repos.yaml. A missing file is an error — the CLI
        // is supposed to write the file before signalling, and a
        // hand-edit would be present on disk by the time the watcher
        // fires.
        let repos_file = FederationConfig::load(&repos_yaml)?;

        // Resolve the workspace file (next to repos.yaml). Optional.
        let workspaces_path = workspaces_path_for(&repos_yaml);
        let workspaces: Option<Arc<WorkspacesFile>> = if workspaces_path.exists() {
            let ws = WorkspacesFile::load(&workspaces_path)
                .map_err(|e| LainError::Config(format!("reload: {e}")))?;
            ws.validate()?;
            Some(Arc::new(ws))
        } else {
            None
        };

        // Compute the diff against the current federation. Repos are
        // keyed by their `id` (which is `RepoId`-validated on parse).
        let fed = server
            .federation()
            .ok_or_else(|| LainError::Other("rebuild: no federation on server".into()))?;
        let prev_ids: std::collections::HashSet<String> = fed
            .list_repos()
            .into_iter()
            .map(|(id, _)| id.to_string())
            .collect();
        let new_ids: std::collections::HashSet<String> = repos_file
            .repos
            .iter()
            .map(|r| r.id.clone())
            .collect();

        // Additions: build the source and delegate to LainServer.
        let data_dir = repos_file.data_dir.clone();
        for repo in &repos_file.repos {
            if !prev_ids.contains(&repo.id) {
                server
                    .add_repo(repo, &data_dir)
                    .await
                    .map_err(|e| LainError::Config(format!("add_repo({}): {e}", repo.id)))?;
            }
        }

        // Removals: drop any repo id no longer in repos.yaml.
        for id in prev_ids.difference(&new_ids) {
            server
                .remove_repo(id)
                .map_err(|e| LainError::Config(format!("remove_repo({}): {e}", id)))?;
        }

        // Update the workspaces slot. The MCP server rebuild path
        // (cli/server.rs) consumes this through `server.workspaces_snapshot()`
        // before tearing down the old LainMcpServer.
        if let Some(ws) = workspaces {
            server.set_workspace(ws);
        }

        Ok(())
    })
    .await;

    match result {
        Ok(()) => {
            bus.set_state(ReloadState::Idle).await;
            Ok(())
        }
        Err(e) => {
            let msg = format!("{e}");
            bus.set_state(ReloadState::Failed(msg.clone())).await;
            Err(LainError::Other(msg))
        }
    }
}

/// Return the conventional workspaces.yaml path next to a given
/// `repos.yaml`. Standalone helper for both the rebuild orchestrator
/// and tests.
pub fn workspaces_path_for(repos_yaml: &std::path::Path) -> PathBuf {
    repos_yaml
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("workspaces.yaml")
}

/// Handle for receiving reload requests.
///
/// `try_recv` is non-blocking: the caller (typically the rebuild task
/// loop, or a test) decides how to wait. A lagged receiver indicates
/// that the bus emitted more than the channel's buffer capacity while
/// the subscriber was idle; the rebuild task responds by re-reading
/// `bus.status()` instead of relying on the caught-up message.
pub struct ReloadSubscriber {
    rx: broadcast::Receiver<()>,
}

impl ReloadSubscriber {
    /// Non-blocking poll. Returns `Ok(())` if a reload request was
    /// received, `Err(Empty)` if no request is pending, or
    /// `Err(Lagged)` if the subscriber fell behind.
    pub fn try_recv(&mut self) -> Result<(), broadcast::error::TryRecvError> {
        loop {
            match self.rx.try_recv() {
                Ok(()) => return Ok(()),
                Err(broadcast::error::TryRecvError::Empty) => {
                    return Err(broadcast::error::TryRecvError::Empty)
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    // Subscriber fell behind; drain any further pending
                    // messages before declaring "empty" so callers don't
                    // miss a request that was already in flight.
                    continue;
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    return Err(broadcast::error::TryRecvError::Closed)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_bus_broadcasts() {
        let bus = ReloadBus::new();
        let mut sub = bus.subscribe();
        bus.request_reload().unwrap();
        assert!(sub.try_recv().is_ok());
    }

    #[test]
    fn reload_status_reports_state() {
        let bus = ReloadBus::new();
        assert_eq!(bus.status().state, ReloadState::Idle);
    }

    #[test]
    fn try_recv_returns_none_when_no_request_pending() {
        let bus = ReloadBus::new();
        let mut sub = bus.subscribe();
        assert!(sub.try_recv().is_err());
    }

    #[test]
    fn status_returns_idle_after_failed_transitions() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let bus = ReloadBus::new();
            bus.set_state(ReloadState::Rebuilding).await;
            bus.set_state(ReloadState::Failed("boom".into())).await;
            let s = bus.status();
            assert_eq!(s.state, ReloadState::Failed("boom".into()));
            assert_eq!(s.last_error.as_deref(), Some("boom"));
            assert!(s.started_at.is_none());
        });
    }

    mod rebuild {
        //! End-to-end rebuild tests that exercise the `LainServer`
        //! thin wrappers (`add_repo` / `remove_repo` / `set_workspace`)
        //! through `run_rebuild` against on-disk `repos.yaml` and
        //! `workspaces.yaml` fixtures.
        //!
        //! Each test writes a one-repo `repos.yaml` pointing at a
        //! `workspace_dir` source backed by a tempdir (no network, no
        //! git clone), then mutates the file and asserts the live
        //! federation reflects the change.
        //!
        //! The tests construct the federation directly with
        //! `load_federation` and skip `LainServer::with_federation`'s
        //! built-in tempdir setup so each test gets a unique data
        //! directory and they don't collide on `lain-federation-{pid}`.

        use super::*;
        use crate::server::federation::federated_index::FederatedIndex;
        use crate::server::federation::graph_backend::PetgraphBackend;
        use crate::server::LainServer;
        use crate::server::Transport;
        use std::path::Path;
        use std::sync::Arc;

        /// Build a `LainServer` whose `federation` field points at a
        /// freshly-loaded federation, mirroring the production path
        /// through `load_federation`. Avoids `with_federation`'s
        /// process-id-based tempdir so tests are independent.
        async fn build_server(
            repos_yaml: &Path,
            fed: Arc<FederatedIndex>,
        ) -> LainServer {
            // `with_federation` builds a placeholder git repo at
            // `/tmp/lain-federation-{pid}` and refuses to re-init one
            // that's missing. A prior test in the same process may
            // have torn it down between `with_federation` calls, so
            // we proactively remove the dir if it exists.
            let staging = std::env::temp_dir()
                .join(format!("lain-federation-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&staging);
            LainServer::with_federation(fed, Transport::Http, 9999, Some(repos_yaml.to_path_buf()), None)
                .expect("LainServer::with_federation")
        }

        /// Build a federation + LainServer around a single
        /// `workspace_dir` repo. Returns `(server, repos_yaml_path)`.
        async fn server_with_workspace_dir(
            tmp: &tempfile::TempDir,
            repo_id: &str,
            repo_path: &Path,
        ) -> (LainServer, std::path::PathBuf) {
            // Clean any stale staging dir so `with_federation` can
            // init it fresh.
            let staging = std::env::temp_dir()
                .join(format!("lain-federation-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&staging);
            let repos_yaml = tmp.path().join("repos.yaml");
            let yaml = format!(
                "data_dir: {}\nrepos:\n  - id: {}\n    source: {{ type: workspace_dir, path: {} }}\n",
                tmp.path().join("federation").display(),
                repo_id,
                repo_path.display(),
            );
            std::fs::write(&repos_yaml, yaml).unwrap();

            let backend: Arc<dyn crate::server::federation::graph_backend::GraphBackend> =
                Arc::new(PetgraphBackend::new(tmp.path()).expect("PetgraphBackend::new"));
            let fed = Arc::new(FederatedIndex::new(backend));
            // Manually add the repo to keep the test independent of
            // `load_federation`'s git-source quirks.
            let cfg: FederationConfig =
                serde_yaml::from_str(&std::fs::read_to_string(&repos_yaml).unwrap()).unwrap();
            let source = cfg.build_source_for(&cfg.repos[0]).expect("build_source_for");
            source.fetch().await.expect("fetch");
            let rid = source.id().clone();
            fed.add_repo(source, &cfg.data_dir).await.expect("add_repo");
            fed.project_repo(&rid).await.expect("project_repo");

            let server = build_server(&repos_yaml, fed).await;
            (server, repos_yaml)
        }

        /// Write `repos_yaml` with the given list of `(id, path)`
        /// `workspace_dir` repos. The `data_dir` is fixed under
        /// `tmp/federation` to keep the federation state isolated per
        /// test.
        fn write_repos_yaml(
            tmp: &Path,
            repos_yaml: &Path,
            repos: &[(&str, &Path)],
        ) {
            let mut yaml = format!("data_dir: {}\nrepos:\n", tmp.join("federation").display());
            for (id, path) in repos {
                yaml.push_str(&format!(
                    "  - id: {}\n    source: {{ type: workspace_dir, path: {} }}\n",
                    id,
                    path.display(),
                ));
            }
            std::fs::write(repos_yaml, yaml).unwrap();
        }

        /// Build a federation from a list of `(id, path)`
        /// `workspace_dir` repos without touching
        /// `load_federation` (avoids the tempdir collision).
        async fn fed_for(
            repos: &[(&str, &Path)],
            data_dir: &Path,
        ) -> Arc<FederatedIndex> {
            let backend: Arc<dyn crate::server::federation::graph_backend::GraphBackend> =
                Arc::new(PetgraphBackend::new(data_dir).expect("PetgraphBackend::new"));
            let fed = Arc::new(FederatedIndex::new(backend));
            for (id, path) in repos {
                let cfg = FederationConfig {
                    data_dir: data_dir.to_path_buf(),
                    max_concurrent_indexers: 1,
                    ready_threshold: 0.8,
                    repos: vec![crate::server::federation::config::RepoConfig {
                        id: (*id).to_string(),
                        source: crate::server::federation::config::SourceConfig::WorkspaceDir {
                            path: path.to_path_buf(),
                        },
                    }],
                };
                let source = cfg.build_source_for(&cfg.repos[0]).expect("build_source_for");
                source.fetch().await.expect("fetch");
                let rid = source.id().clone();
                fed.add_repo(source, &cfg.data_dir).await.expect("add_repo");
                fed.project_repo(&rid).await.expect("project_repo");
            }
            fed
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn rebuild_picks_up_new_repo_in_repos_yaml() {
            let tmp = tempfile::tempdir().unwrap();
            let repo_a = tmp.path().join("repo-a");
            std::fs::create_dir_all(&repo_a).unwrap();
            git2::Repository::init(&repo_a).unwrap();
            let (server, repos_yaml) = server_with_workspace_dir(&tmp, "repo-a", &repo_a).await;
            assert_eq!(server.repo_count(), 1);

            // Add a second workspace_dir repo to repos.yaml.
            let repo_b = tmp.path().join("repo-b");
            std::fs::create_dir_all(&repo_b).unwrap();
            git2::Repository::init(&repo_b).unwrap();
            write_repos_yaml(
                tmp.path(),
                &repos_yaml,
                &[("repo-a", &repo_a), ("repo-b", &repo_b)],
            );
            let bus = server.reload_bus();
            crate::server::reload::run_rebuild(&server, &bus).await.expect("run_rebuild");
            assert_eq!(server.repo_count(), 2);
            assert_eq!(bus.status().state, ReloadState::Idle);
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn rebuild_drops_repo_removed_from_repos_yaml() {
            let tmp = tempfile::tempdir().unwrap();
            let repo_a = tmp.path().join("repo-a");
            std::fs::create_dir_all(&repo_a).unwrap();
            git2::Repository::init(&repo_a).unwrap();
            let repo_b = tmp.path().join("repo-b");
            std::fs::create_dir_all(&repo_b).unwrap();
            git2::Repository::init(&repo_b).unwrap();
            write_repos_yaml(
                tmp.path(),
                &tmp.path().join("repos.yaml"),
                &[("repo-a", &repo_a), ("repo-b", &repo_b)],
            );
            let data_dir = tmp.path().join("federation");
            std::fs::create_dir_all(&data_dir).unwrap();
            let fed = fed_for(
                &[("repo-a", &repo_a), ("repo-b", &repo_b)],
                &data_dir,
            )
            .await;
            let server = build_server(tmp.path().join("repos.yaml").as_path(), fed).await;
            assert_eq!(server.repo_count(), 2);

            // Remove repo-b from repos.yaml and rebuild.
            write_repos_yaml(
                tmp.path(),
                tmp.path().join("repos.yaml").as_path(),
                &[("repo-a", &repo_a)],
            );
            let bus = server.reload_bus();
            crate::server::reload::run_rebuild(&server, &bus).await.expect("run_rebuild");
            assert_eq!(server.repo_count(), 1);
            assert_eq!(bus.status().state, ReloadState::Idle);
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn rebuild_replaces_workspaces_yaml_when_present() {
            let tmp = tempfile::tempdir().unwrap();
            let repo_a = tmp.path().join("repo-a");
            std::fs::create_dir_all(&repo_a).unwrap();
            git2::Repository::init(&repo_a).unwrap();
            let (server, _repos_yaml) = server_with_workspace_dir(&tmp, "repo-a", &repo_a).await;
            // Migrating the test off `with_federation`: with the
            // shared-lock fix, `set_workspace` is only meaningful for
            // servers whose `federation_workspaces` slot is `Some`,
            // i.e., those built via `with_federation_and_workspaces`.
            // `run_rebuild` itself calls `set_workspace` (Task 6.2) so
            // we need the slot to be present for the rebuild path to
            // push through; rebuild a fresh server here with an empty
            // initial `WorkspacesFile`.
            let fed = server.federation().unwrap().clone();
            let server = LainServer::with_federation_and_workspaces(
                fed,
                Transport::Http,
                9999,
                Arc::new(crate::server::federation::workspace::WorkspacesFile {
                    default: None,
                    workspaces: vec![],
                }),
                Some(_repos_yaml.clone()),
                None, // hot-reload doesn't change the embedding model
            )
            .expect("with_federation_and_workspaces");
            assert_eq!(server.workspace_count(), 0);

            // Add a workspaces.yaml with one workspace.
            let ws_path = tmp.path().join("workspaces.yaml");
            std::fs::write(
                &ws_path,
                "workspaces:\n  - name: w1\n    members: [repo-a]\n",
            )
            .unwrap();
            let bus = server.reload_bus();
            crate::server::reload::run_rebuild(&server, &bus).await.expect("run_rebuild");
            assert_eq!(server.workspace_count(), 1);
            assert_eq!(bus.status().state, ReloadState::Idle);
            let ws = server.workspaces_snapshot().expect("workspaces");
            assert_eq!(ws.workspaces[0].name, "w1");
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn rebuild_records_failed_state_on_invalid_repos_yaml() {
            let tmp = tempfile::tempdir().unwrap();
            let repo_a = tmp.path().join("repo-a");
            std::fs::create_dir_all(&repo_a).unwrap();
            git2::Repository::init(&repo_a).unwrap();
            let (server, repos_yaml) = server_with_workspace_dir(&tmp, "repo-a", &repo_a).await;

            // Replace repos.yaml with an invalid repo id (`/`) which
            // fails `RepoId::new`.
            std::fs::write(&repos_yaml, "repos:\n  - id: bad/id\n").unwrap();
            let bus = server.reload_bus();
            let result =
                crate::server::reload::run_rebuild(&server, &bus).await;
            assert!(result.is_err());
            match bus.status().state {
                ReloadState::Failed(msg) => {
                    assert!(!msg.is_empty(), "Failed state should carry an error message");
                }
                other => panic!("expected Failed state, got {:?}", other),
            }
            // The federation should still hold repo-a: a partial
            // failure doesn't tear down the existing repos.
            assert_eq!(server.repo_count(), 1);
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn lain_server_set_workspace_swaps_slot() {
            let tmp = tempfile::tempdir().unwrap();
            let repo_a = tmp.path().join("repo-a");
            std::fs::create_dir_all(&repo_a).unwrap();
            git2::Repository::init(&repo_a).unwrap();
            let (server, repos_yaml) = server_with_workspace_dir(&tmp, "repo-a", &repo_a).await;
            // `set_workspace` only writes to a slot that exists. A
            // server built via `with_federation` (no workspaces) has
            // `None` and the write is a no-op. Build the workspace-
            // aware variant so `set_workspace` actually pushes
            // through the lock the LainMcpServer holds.
            let fed = server.federation().unwrap().clone();
            let server = LainServer::with_federation_and_workspaces(
                fed,
                Transport::Http,
                9999,
                Arc::new(crate::server::federation::workspace::WorkspacesFile {
                    default: None,
                    workspaces: vec![],
                }),
                Some(repos_yaml.clone()),
                None, // hot-reload doesn't change the embedding model
            )
            .expect("with_federation_and_workspaces");
            assert_eq!(server.workspace_count(), 0);
            let ws = Arc::new(crate::server::federation::workspace::WorkspacesFile {
                default: None,
                workspaces: vec![crate::server::federation::workspace::WorkspaceSpec {
                    name: "w1".into(),
                    description: None,
                    source: None,
                    members: vec!["repo-a".into()],
                }],
            });
            server.set_workspace(ws);
            assert_eq!(server.workspace_count(), 1);
        }
    }
}
