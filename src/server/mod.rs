//! Lain server orchestration
//!
//! Wires together all components: graph, LSP, git, MCP

pub mod ingestion;
pub mod scan;
pub mod jobs;

use crate::error::LainError;
use crate::federation::federated_index::FederatedIndex;
use crate::graph::GraphDatabase;
use crate::lsp::LspPool;
use crate::nlp::NlpEmbedder;
use crate::overlay::VolatileOverlay;
use crate::tools::ToolExecutor;
use crate::tuning::{load_tuning_config, TuningConfig};
use crate::git::GitSensor;
use std::path::{Path, PathBuf};
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
    pub git: Arc<Mutex<GitSensor>>,
    pub lsp_pool: Arc<LspPool>,
    pub tool_executor: ToolExecutor,
    pub tuning: Arc<TuningConfig>,
    /// Federation handle. `Some` for federation-mode servers (constructed
    /// via `with_federation`); `None` for single-workspace servers
    /// (constructed via `new`).
    federation: Option<Arc<FederatedIndex>>,
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
            NlpEmbedder::new_with_paths(model_path, &tokenizer_path)?
        } else {
            NlpEmbedder::new()?
        };

        if embedder.is_stub() {
            info!("NLP embedder running in stub mode - semantic search unavailable");
        }

        let git = Arc::new(Mutex::new(GitSensor::new(workspace)?));
        let lsp_pool = Arc::new(LspPool::new(workspace, tuning.ingestion.lsp_pool_size)?);

        let tool_executor = ToolExecutor::new(
            graph.clone(),
            overlay.clone(),
            embedder.clone(),
            Arc::clone(&git),
            Arc::clone(&lsp_pool),
            Arc::clone(&tuning),
        );

        info!("Lain server initialized");
        Ok(Self {
            config,
            graph,
            overlay,
            embedder,
            git,
            lsp_pool,
            tool_executor,
            tuning,
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
            Arc::clone(&git),
            Arc::clone(&lsp_pool),
            Arc::clone(&tuning),
        );

        // Build the federation-aware MCP server eagerly so any wiring
        // problems surface at construction time. We don't store the
        // `LainMcpServer` itself — it's a thin wrapper over `tool_executor`
        // plus `federation`, both of which we already hold — and `serve()`
        // rebuilds it before consuming self.
        let _mcp = crate::mcp::LainMcpServer::with_federation(
            tool_executor.clone(),
            Arc::clone(&federation),
        );

        info!("Lain federation server initialized");
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
            federation: Some(federation),
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

        let mcp = crate::mcp::LainMcpServer::with_federation(self.tool_executor, federation);
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

    pub fn is_git_repo(&self) -> bool {
        self.git.lock().is_valid()
    }

    pub async fn run_mcp_server(&mut self) -> Result<(), LainError> {
        info!("Starting MCP server using rust-mcp-sdk");

        let mcp_server = crate::mcp::LainMcpServer::new(self.tool_executor.clone());
        mcp_server.run_stdio().await.map_err(|e| {
            LainError::Mcp(format!("MCP server error: {}", e))
        })?;

        Ok(())
    }

    pub async fn shutdown(&self) {
        info!("Shutting down Lain server...");
        self.lsp_pool.shutdown_all().await;
    }
}
