//! Lain server orchestration
//!
//! Wires together all components: graph, LSP, git, MCP

pub mod ingestion;
pub mod scan;
pub mod jobs;

use crate::error::LainError;
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
        })
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
