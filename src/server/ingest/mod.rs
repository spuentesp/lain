//! Ingest pipeline + Lain server orchestration.
//!
//! `LainServer` wires together every component the MCP layer needs: the
//! persistent graph, the volatile overlay, the embedder + cross-encoder,
//! the git sensor, the LSP pool, and the tool executor. It owns the
//! federation handle (set by `with_federation*`) and the background-sync
//! job. The three sibling modules — `ingestion`, `jobs`, `scan` — carry
//! the ingest pipeline itself.

pub mod ingestion;
pub mod scan;
pub mod jobs;

use crate::server::error::LainError;
use crate::server::federation::federated_index::FederatedIndex;
use crate::server::federation::workspace::WorkspacesFile;
use crate::server::graph::GraphDatabase;
use crate::server::lsp::LspPool;
use crate::server::nlp::{CrossEncoder, NlpEmbedder};
use crate::server::overlay::{OverlayDiff, VolatileOverlay};
use crate::server::tools::ToolExecutor;
use crate::server::tuning::{load_tuning_config, TuningConfig};
use crate::server::git::GitSensor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use parking_lot::Mutex;
use tracing::info;

/// Server configuration
#[derive(Clone)]
pub struct LainConfig {
    pub workspace: PathBuf,
    pub memory_path: PathBuf,
}

/// MCP transport for federation-mode servers. Stdio for local agents,
/// Http for network-reachable deployments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Stdio,
    Http,
}

/// Main Lain server
#[derive(Clone)]
pub struct LainServer {
    pub config: LainConfig,
    pub graph: GraphDatabase,
    pub overlay: VolatileOverlay,
    pub embedder: NlpEmbedder,
    pub cross_encoder: CrossEncoder,
    pub git: Arc<Mutex<GitSensor>>,
    pub lsp_pool: Arc<LspPool>,
    pub tool_executor: ToolExecutor,
    pub tuning: Arc<TuningConfig>,
    /// Monotonic counter used as the `revision` field of every
    /// `OverlayDiff` this process broadcasts.
    overlay_revision: Arc<AtomicU64>,
    /// Federation handle. `Some` for federation-mode servers (constructed
    /// via `with_federation`); `None` for single-workspace servers
    /// (constructed via `new`).
    federation: Option<Arc<FederatedIndex>>,
    /// Workspaces file passed to `LainMcpServer` when `with_federation_and_workspaces`
    /// is used. `Some` when a workspace is active; `None` for the
    /// all-repos path (no workspaces.yaml).
    federation_workspaces: Option<Arc<WorkspacesFile>>,
    /// Transport chosen at `with_federation` time. Consumed by `serve`.
    federation_transport: Option<Transport>,
    /// Port chosen at `with_federation` time. Consumed by `serve`.
    federation_port: Option<u16>,
}

impl LainServer {
    pub fn new(workspace: &Path, memory_path: &Path, embedding_model: Option<&Path>) -> Result<Self, LainError> {
        let config = LainConfig {
            workspace: workspace.to_path_buf(),
            memory_path: memory_path.to_path_buf(),
        };

        let tuning = Arc::new(load_tuning_config(workspace));

        let graph = GraphDatabase::new(memory_path)?;
        let overlay = VolatileOverlay::new();

        let embedder = if let Some(model_path) = embedding_model {
            let tokenizer_path = model_path.parent().map(|p| p.join("tokenizer.json"))
                .unwrap_or_else(|| PathBuf::from("tokenizer.json"));
            crate::server::nlp::NlpEmbedder::with_max_threads(
                model_path,
                &tokenizer_path,
                tuning.ingestion.nlp_max_threads,
            )?
        } else {
            // No --embedding-model CLI arg; fall back to LAIN_EMBEDDING_MODEL
            // env var (handled inside NlpEmbedder::new_with_threads).
            crate::server::nlp::NlpEmbedder::new_with_threads(tuning.ingestion.nlp_max_threads)?
        };

        if embedder.is_stub() {
            info!("NLP embedder running in stub mode - semantic search unavailable");
        }

        // Cross-encoder sits beside the bi-encoder model by default.
        // Override via LAIN_CROSS_ENCODER env var.
        let cross_dir = std::env::var("LAIN_CROSS_ENCODER")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_default();
                PathBuf::from(home).join(".local/lain/models/cross-encoder")
            });
        let cross_encoder = CrossEncoder::from_dir_with_threads(
            &cross_dir,
            tuning.ingestion.nlp_max_threads,
        );
        if cross_encoder.is_active() {
            info!("Cross-encoder reranker active (from {:?})", cross_dir);
        } else {
            info!("Cross-encoder reranker disabled (no model at {:?})", cross_dir);
        }

        let git = Arc::new(Mutex::new(GitSensor::new(workspace)?));
        let lsp_pool = Arc::new(LspPool::new(workspace, tuning.ingestion.lsp_pool_size)?);

        let tool_executor = ToolExecutor::new(
            graph.clone(),
            overlay.clone(),
            embedder.clone(),
            cross_encoder.clone(),
            Arc::clone(&git),
            Arc::clone(&lsp_pool),
            Arc::clone(&tuning),
            workspace.to_path_buf(),
        );

        info!("Lain server initialized");
        Ok(Self {
            config,
            graph,
            overlay,
            embedder,
            federation_workspaces: None,
            cross_encoder,
            git,
            lsp_pool,
            tool_executor,
            tuning,
            overlay_revision: Arc::new(AtomicU64::new(0)),
            federation: None,
            federation_transport: None,
            federation_port: None,
        })
    }

    /// Federation-aware constructor. Builds a `LainServer` whose tool surface
    /// is the federation's (`list_repos`, `get_repo_info`, `search_org`,
    /// `get_federation_health`, `get_cross_repo_blast_radius*`).
    ///
    /// Unlike `LainServer::new`, this does **not** point at a single
    /// workspace: the federation manages N repos, so per-repo
    /// git/lsp/embedder/graph components are placeholders seeded from a
    /// throwaway temp git repo (mirrors `cmds/server.rs`'s pre-Task-24
    /// behavior). Federation tools are dispatched in `mcp/handler.rs`
    /// before they touch the executor's underlying services, so the
    /// placeholder wiring is sufficient.
    ///
    /// Pair with `serve()` to start the MCP loop on the chosen transport.
    pub fn with_federation(
        federation: Arc<FederatedIndex>,
        transport: Transport,
        port: u16,
    ) -> Result<Self, LainError> {
        // Build a minimal executor — same trick as `cmds/server.rs`'s
        // `build_minimal_executor`. Federation tools never reach the
        // executor's underlying services, but `LainMcpServer::with_federation`
        // still requires a constructed one.
        let ws = std::env::temp_dir().join(format!("lain-federation-{}", std::process::id()));
        std::fs::create_dir_all(&ws)?;
        if !ws.join(".git").exists() {
            git2::Repository::init(&ws)?;
        }
        let mem_dir = ws.join(".lain");
        std::fs::create_dir_all(&mem_dir)?;
        let mem_path = mem_dir.join("graph.bin");
        // Ensure no stale file from a previous run leaks in.
        let _ = std::fs::remove_file(&mem_path);

        let graph = GraphDatabase::new(&mem_path)?;
        let overlay = VolatileOverlay::new();
        let embedder = NlpEmbedder::new()?;
        if embedder.is_stub() {
            info!("NLP embedder running in stub mode (federation placeholder)");
        }
        let git = Arc::new(Mutex::new(GitSensor::new(&ws)?));
        let lsp_pool = Arc::new(LspPool::new(&ws, 1)?);
        let tuning = Arc::new(load_tuning_config(&ws));

        let tool_executor = ToolExecutor::new(
            graph.clone(),
            overlay.clone(),
            embedder.clone(),
            crate::server::nlp::CrossEncoder::from_dir(&ws),
            Arc::clone(&git),
            Arc::clone(&lsp_pool),
            Arc::clone(&tuning),
            ws.to_path_buf(),
        );

        // Build the federation-aware MCP server eagerly so any wiring
        // problems surface at construction time. We don't store the
        // `LainMcpServer` itself — it's a thin wrapper over `tool_executor`
        // plus `federation`, both of which we already hold — and `serve()`
        // rebuilds it before consuming self.
        let _mcp = crate::server::mcp::handler::LainMcpServer::with_federation(
            tool_executor.clone(),
            Arc::clone(&federation),
        );

        info!("Lain federation server initialized");
        let cross_encoder = crate::server::nlp::CrossEncoder::from_dir(&ws);

        Ok(Self {
            config: LainConfig {
                workspace: ws,
                memory_path: mem_path,
            },
            graph,
            overlay,
            embedder,
            git,
            lsp_pool,
            tool_executor,
            tuning,
            cross_encoder,
            overlay_revision: Arc::new(AtomicU64::new(0)),
            federation: Some(federation),
            federation_workspaces: None,
            federation_transport: Some(transport),
            federation_port: Some(port),
        })
    }

    /// Same as `with_federation` but also registers the workspace MCP
    /// tools (`list_workspaces`, `get_active_workspace`, `get_workspace`,
    /// `get_workspace_graph`) against the supplied `WorkspacesFile`. Used
    /// when the server is started with `--workspace <name>` and a
    /// `workspaces.yaml` is present next to `repos.yaml`.
    pub fn with_federation_and_workspaces(
        federation: Arc<FederatedIndex>,
        transport: Transport,
        port: u16,
        workspaces: Arc<WorkspacesFile>,
    ) -> Result<Self, LainError> {
        // Mostly the same wiring as `with_federation`. The differences:
        // we store the workspaces file in `federation_workspaces`, and we
        // build the LainMcpServer with the workspaces-aware constructor
        // eagerly so any wiring problems surface at construction time.
        let ws = std::env::temp_dir().join(format!("lain-federation-{}", std::process::id()));
        std::fs::create_dir_all(&ws)?;
        if !ws.join(".git").exists() {
            git2::Repository::init(&ws)?;
        }
        let mem_dir = ws.join(".lain");
        std::fs::create_dir_all(&mem_dir)?;
        let mem_path = mem_dir.join("graph.bin");
        let _ = std::fs::remove_file(&mem_path);

        let graph = GraphDatabase::new(&mem_path)?;
        let overlay = VolatileOverlay::new();
        let embedder = NlpEmbedder::new()?;
        if embedder.is_stub() {
            info!("NLP embedder running in stub mode (federation placeholder)");
        }
        let git = Arc::new(Mutex::new(GitSensor::new(&ws)?));
        let lsp_pool = Arc::new(LspPool::new(&ws, 1)?);
        let tuning = Arc::new(load_tuning_config(&ws));

        let tool_executor = ToolExecutor::new(
            graph.clone(),
            overlay.clone(),
            embedder.clone(),
            crate::server::nlp::CrossEncoder::from_dir(&ws),
            Arc::clone(&git),
            Arc::clone(&lsp_pool),
            Arc::clone(&tuning),
            ws.to_path_buf(),
        );

        let _mcp = crate::server::mcp::handler::LainMcpServer::with_federation_and_workspaces(
            tool_executor.clone(),
            Arc::clone(&federation),
            Arc::clone(&workspaces),
        );

        info!("Lain federation server initialized with workspaces");
        let cross_encoder = crate::server::nlp::CrossEncoder::from_dir(&ws);

        Ok(Self {
            config: LainConfig {
                workspace: ws,
                memory_path: mem_path,
            },
            graph,
            overlay,
            embedder,
            git,
            lsp_pool,
            tool_executor,
            tuning,
            cross_encoder,
            overlay_revision: Arc::new(AtomicU64::new(0)),
            federation: Some(federation),
            federation_workspaces: Some(workspaces),
            federation_transport: Some(transport),
            federation_port: Some(port),
        })
    }

    /// Federation accessor. Returns `None` for single-workspace servers.
    pub fn federation(&self) -> Option<&Arc<FederatedIndex>> {
        self.federation.as_ref()
    }

    /// Consume the server and run the federation-mode MCP loop on the
    /// transport chosen at construction time. Errors out if the server
    /// was not built via `with_federation`.
    pub async fn serve(self) -> Result<(), LainError> {
        let federation = self.federation.ok_or_else(|| {
            LainError::Other(
                "LainServer::serve() called on a non-federation server (use LainServer::new for single-workspace)".into(),
            )
        })?;
        let transport = self.federation_transport.ok_or_else(|| {
            LainError::Other("LainServer::serve(): missing transport (internal)".into())
        })?;
        let port = self.federation_port.unwrap_or(9999);

        let mcp = match self.federation_workspaces {
            Some(ws) => crate::server::mcp::handler::LainMcpServer::with_federation_and_workspaces(
                self.tool_executor, federation, ws,
            ),
            None => crate::server::mcp::handler::LainMcpServer::with_federation(self.tool_executor, federation),
        };
        match transport {
            Transport::Http => mcp
                .run_http(port)
                .await
                .map_err(|e| LainError::Mcp(format!("HTTP transport: {e}"))),
            Transport::Stdio => mcp
                .run_stdio()
                .await
                .map_err(|e| LainError::Mcp(format!("stdio transport: {e}"))),
        }
    }

    pub fn clone_for_background(&self) -> Self {
        self.clone()
    }

    /// Allocate the next overlay-diff revision id. Sidecars use this to
    /// detect drops in the broadcast bus.
    pub fn next_revision(&self) -> crate::server::overlay::RevisionId {
        self.overlay_revision.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Broadcast a single node insertion. Sidecars pull these diffs from
    /// the broadcast bus and merge them into their in-memory overlay.
    /// Only fires when this process is an *owner* — the sidecar has its
    /// graph opened read-only (Task 3) and never reaches the call sites
    /// below `check_writable`, so it cannot broadcast by accident.
    pub fn broadcast_overlay_insert(
        &self,
        node: crate::server::schema::GraphNode,
    ) {
        crate::server::overlay::broadcast_overlay_diff(OverlayDiff {
            revision: self.next_revision(),
            added: vec![node],
            removed: vec![],
            updated: vec![],
        });
    }

    pub fn is_git_repo(&self) -> bool {
        self.git.lock().is_valid()
    }

    pub async fn shutdown(&self) {
        info!("Shutting down Lain server...");
        self.lsp_pool.shutdown_all().await;
    }
}
