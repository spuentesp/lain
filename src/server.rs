//! Lain server orchestration
//!
//! Wires together all components: graph, LSP, git, MCP

use crate::error::LainError;
use crate::git::GitSensor;
use crate::graph::GraphDatabase;
use crate::lsp::{LspMultiplexer, HierarchicalSymbol};
use crate::mcp::McpHandler;
use crate::nlp::NlpEmbedder;
use crate::overlay::VolatileOverlay;
use crate::schema::{GraphEdge, GraphNode, NodeType, EdgeType};
use crate::tools::ToolExecutor;
use rmcp::service::ServiceExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use parking_lot::Mutex;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, info, warn};

/// Server configuration
#[derive(Clone)]
pub struct LainConfig {
    pub workspace: PathBuf,
    pub memory_path: PathBuf,
}

/// Main Lain server
pub struct LainServer {
    pub config: LainConfig,
    pub graph: GraphDatabase,
    pub overlay: VolatileOverlay,
    pub embedder: NlpEmbedder,
    pub git: Arc<Mutex<GitSensor>>,
    pub lsp: Arc<AsyncMutex<LspMultiplexer>>,
    pub tool_executor: ToolExecutor,
}

impl LainServer {
    pub fn new(workspace: &Path, memory_path: &Path) -> Result<Self, LainError> {
        let config = LainConfig {
            workspace: workspace.to_path_buf(),
            memory_path: memory_path.to_path_buf(),
        };

        let graph = GraphDatabase::new(memory_path)?;
        let overlay = VolatileOverlay::new();
        let embedder = NlpEmbedder::new()?;
        let git = Arc::new(Mutex::new(GitSensor::new(workspace)?));
        let lsp = Arc::new(AsyncMutex::new(LspMultiplexer::new(workspace)?));

        let tool_executor = ToolExecutor::new(
            graph.clone(),
            overlay.clone(),
            embedder.clone(),
            Arc::clone(&git),
            Arc::clone(&lsp),
        );

        info!("Lain server initialized");
        Ok(Self {
            config,
            graph,
            overlay,
            embedder,
            git,
            lsp,
            tool_executor,
        })
    }

    pub fn is_git_repo(&self) -> bool {
        self.git.lock().is_valid()
    }

    pub async fn build_core_memory(&mut self) -> Result<(), LainError> {
        let start_time = std::time::Instant::now();
        let last_commit = self.graph.get_last_commit()?;
        let (latest_commit, latest_time) = self.git.lock().get_latest_commit_info()?;

        if let Some(ref last) = last_commit {
            if last == &latest_commit {
                info!("Core memory is up to date with commit {}", last);
                return Ok(());
            }
        }

        info!("Building core memory");
        let files = if let Some(ref last) = last_commit {
            info!("Incremental update since {}", last);
            self.git.lock().get_changed_files_since(last)?
        } else {
            info!("Full repository scan");
            self.git.lock().get_all_tracked_files()?
        };
        
        let total_files = files.len();
        if total_files == 0 {
            info!("No files to process.");
            self.graph.set_last_commit(latest_commit)?;
            return Ok(());
        }

        let lsp_sync_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let mut set = tokio::task::JoinSet::new();
        
        for file in files.iter().take(1000).cloned() {
            let lsp = Arc::clone(&self.lsp);
            let graph = self.graph.clone();
            let workspace = self.config.workspace.clone();
            let commit_hash = latest_commit.clone();
            let git_time = latest_time;
            
            set.spawn(async move {
                process_file_internal(file, workspace, lsp, graph, lsp_sync_time, git_time, commit_hash).await
            });
        }

        let mut processed = 0;
        while let Some(res) = set.join_next().await {
            processed += 1;
            if let Err(e) = res {
                warn!("Task join error during indexing: {}", e);
            } else if let Some(Err(e)) = res.ok() {
                warn!("File processing error: {}", e);
            }
        }
        
        self.graph.set_last_commit(latest_commit)?;
        
        let duration = start_time.elapsed();
        info!("Core memory built successfully. Processed {}/{} files in {:?}", processed, total_files, duration);
        Ok(())
    }

    pub async fn sync_volatile_overlay(&mut self) -> Result<(), LainError> {
        self.overlay.clear();
        let changes = self.git.lock().get_uncommitted_changes()?;

        for change in &changes {
            if let Err(e) = self.process_change(&change.path).await {
                warn!("Failed to process change {:?}: {}", change.path, e);
            }
        }
        Ok(())
    }

    async fn process_change(&mut self, path: &Path) -> Result<(), LainError> {
        let symbols = {
            let mut lsp = self.lsp.lock().await;
            match lsp.get_document_symbols_hierarchical(path).await {
                Ok(s) => s,
                Err(e) => {
                    debug!("No LSP symbols for changed file {:?}: {}", path, e);
                    return Ok(());
                }
            }
        };
        for symbol in symbols {
            self.overlay.insert_node(symbol.node);
        }
        Ok(())
    }

    pub async fn run_mcp_server(&mut self) -> Result<(), LainError> {
        info!("Starting MCP server on stdio");

        let handler = McpHandler::new(self.tool_executor.clone());
        let (stdin, stdout) = rmcp::transport::stdio();

        handler.serve((stdin, stdout)).await.map_err(|e| {
            LainError::Mcp(format!("MCP server error: {}", e))
        })?;

        Ok(())
    }
}

/// Standalone file processing function for parallelism
async fn process_file_internal(
    path: PathBuf, 
    workspace: PathBuf,
    lsp_mux: Arc<AsyncMutex<LspMultiplexer>>,
    graph: GraphDatabase,
    lsp_sync: i64,
    git_sync: i64,
    commit_hash: String,
) -> Result<(), LainError> {
    let symbols = {
        let mut lsp = lsp_mux.lock().await;
        match lsp.get_document_symbols_hierarchical(&path).await {
            Ok(s) => s,
            Err(e) => {
                debug!("No LSP symbols for {:?}: {}", path, e);
                return Ok(());
            }
        }
    };

    let relative_path = path.strip_prefix(&workspace)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string_lossy().to_string());

    // 1. Create/Ensure Module hierarchy for directories
    let mut current_parent_id = None;
    if let Some(parent_dir) = Path::new(&relative_path).parent() {
        let mut components = Vec::new();
        for component in parent_dir.components() {
            components.push(component.as_os_str().to_string_lossy().to_string());
            let current_module_path = components.join("/");
            
            let mut module_node = GraphNode::new(
                NodeType::Namespace,
                component.as_os_str().to_string_lossy().to_string(),
                current_module_path.clone(),
            );
            module_node.last_lsp_sync = Some(lsp_sync);
            module_node.last_git_sync = Some(git_sync);
            module_node.commit_hash = Some(commit_hash.clone());
            
            graph.insert_node(&module_node)?;
            
            if let Some(prev_id) = current_parent_id {
                let edge = GraphEdge::new(EdgeType::Contains, prev_id, module_node.id.clone());
                graph.insert_edge(&edge)?;
            }
            current_parent_id = Some(module_node.id);
        }
    }

    // 2. Create File node
    let mut file_node = GraphNode::new(
        NodeType::File,
        path.file_name().unwrap_or_default().to_string_lossy().to_string(),
        relative_path,
    );
    file_node.last_lsp_sync = Some(lsp_sync);
    file_node.last_git_sync = Some(git_sync);
    file_node.commit_hash = Some(commit_hash.clone());
    
    graph.insert_node(&file_node)?;

    if let Some(parent_id) = current_parent_id {
        let edge = GraphEdge::new(EdgeType::Contains, parent_id, file_node.id.clone());
        graph.insert_edge(&edge)?;
    }

    // 3. Process symbols recursively
    for symbol in symbols {
        process_symbol_tree(&graph, &file_node.id, symbol, lsp_sync, git_sync, commit_hash.clone()).await?;
    }

    Ok(())
}

#[async_recursion::async_recursion]
async fn process_symbol_tree(
    graph: &GraphDatabase,
    parent_id: &str,
    symbol: HierarchicalSymbol,
    lsp_sync: i64,
    git_sync: i64,
    commit_hash: String,
) -> Result<(), LainError> {
    let mut node = symbol.node;
    node.last_lsp_sync = Some(lsp_sync);
    node.last_git_sync = Some(git_sync);
    node.commit_hash = Some(commit_hash.clone());
    
    graph.insert_node(&node)?;
    
    let edge = GraphEdge::new(EdgeType::Contains, parent_id.to_string(), node.id.clone());
    graph.insert_edge(&edge)?;

    for child in symbol.children {
        process_symbol_tree(graph, &node.id, child, lsp_sync, git_sync, commit_hash.clone()).await?;
    }
    
    Ok(())
}
