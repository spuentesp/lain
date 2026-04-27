use crate::error::LainError;
use crate::schema::{GraphEdge, GraphNode, NodeType, EdgeType};
use crate::lsp::{LspMultiplexer, HierarchicalSymbol, ReferenceLocation};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;
use tracing::debug;

/// A raw call/type-usage reference from tree-sitter, not yet resolved to node IDs.
pub struct StaticFileRef {
    pub file_path: String,
    pub source_line: u32,
    pub target_name: String,
    pub edge_type: EdgeType,
}

/// A string literal that could indicate cross-boundary coupling
pub struct PatternRef {
    pub file_path: String,
    pub source_line: u32,
    pub value: String,
}

/// Result of a single file's structural scan
pub struct FileScanResult {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub external_references: Vec<(String, ReferenceLocation)>, // source_node_id, reference
    pub static_refs: Vec<StaticFileRef>,
    pub pattern_refs: Vec<PatternRef>,
}

/// Pure structural scan without side effects (Map)
pub async fn scan_file_structure(
    path: PathBuf, 
    workspace: PathBuf,
    lsp_mux: Arc<AsyncMutex<LspMultiplexer>>,
    lsp_sync: i64,
    git_sync: i64,
    commit_hash: String,
) -> Result<FileScanResult, LainError> {
    let relative_path = path.strip_prefix(&workspace)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string_lossy().to_string());

    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut external_references = Vec::new();

    // 1. Module hierarchy for directories
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
            
            let node_id = module_node.id.clone();
            nodes.push(module_node);
            
            if let Some(prev_id) = current_parent_id {
                edges.push(GraphEdge::new(EdgeType::Contains, prev_id, node_id.clone()));
            }
            current_parent_id = Some(node_id);
        }
    }

    // 2. File node
    let mut file_node = GraphNode::new(
        NodeType::File,
        path.file_name().unwrap_or_default().to_string_lossy().to_string(),
        relative_path.clone(),
    );
    file_node.last_lsp_sync = Some(lsp_sync);
    file_node.last_git_sync = Some(git_sync);
    file_node.commit_hash = Some(commit_hash.clone());
    
    let file_id = file_node.id.clone();
    nodes.push(file_node);

    if let Some(parent_id) = current_parent_id {
        edges.push(GraphEdge::new(EdgeType::Contains, parent_id, file_id.clone()));
    }

    // 3. Fetch all references for this file while we hold the lock (prevents nested-lock deadlock)
    let file_refs: Vec<ReferenceLocation> = {
        let mut lsp = lsp_mux.lock().await;
        lsp.get_references(&path, 0, 0).await.unwrap_or_default()
    };

    // Collect (node_id, reference) tuples for deferred resolution
    for r in &file_refs {
        external_references.push((file_id.clone(), r.clone()));
    }

    // 4. Recursive symbols (no more per-symbol lock acquisition)
    let symbols_result = {
        let mut lsp = lsp_mux.lock().await;
        lsp.get_document_symbols_hierarchical(&path).await
    };

    match symbols_result {
        Ok(symbols) => {
            for symbol in symbols {
                process_symbol_recursive_enriched(
                    &mut nodes,
                    &mut edges,
                    &file_id,
                    symbol,
                    lsp_sync,
                    git_sync,
                    commit_hash.clone()
                ).await;
            }
        },
        Err(e) => {
            debug!("No LSP symbols for {:?}: {}", path, e);
            // We still return the file and module nodes
        }
    }

    // Tree-sitter static analysis: extract call, type-usage refs, and string literals from source
    // Read file once — reuse content for both extractors
    let (static_refs, pattern_refs) = if let Ok(content) = tokio::fs::read_to_string(&path).await {
        let path_str = path.to_string_lossy().to_string();
        let static_refs: Vec<StaticFileRef> = crate::treesitter::extract_refs(&path, &content)
            .into_iter()
            .map(|r| StaticFileRef {
                file_path: path_str.clone(),
                source_line: r.source_line,
                target_name: r.target_name,
                edge_type: r.edge_type,
            })
            .collect();
        let pattern_refs: Vec<PatternRef> = crate::treesitter::extract_strings(&path, &content)
            .into_iter()
            .map(|r| PatternRef {
                file_path: path_str.clone(),
                source_line: r.source_line,
                value: r.value,
            })
            .collect();
        (static_refs, pattern_refs)
    } else {
        (vec![], vec![])
    };

    Ok(FileScanResult { nodes, edges, external_references, static_refs, pattern_refs })
}

/// Scan multiple files in a single task (batch processing for reduced task overhead)
pub async fn scan_file_batch(
    paths: Vec<PathBuf>,
    workspace: PathBuf,
    lsp_mux: Arc<AsyncMutex<LspMultiplexer>>,
    lsp_sync: i64,
    git_sync: i64,
    commit_hash: String,
) -> Vec<Result<FileScanResult, LainError>> {
    let mut results = Vec::with_capacity(paths.len());
    for path in paths {
        let result = scan_file_structure(
            path,
            workspace.clone(),
            Arc::clone(&lsp_mux),
            lsp_sync,
            git_sync,
            commit_hash.clone(),
        ).await;
        results.push(result);
    }
    results
}

#[allow(clippy::too_many_arguments)]
#[async_recursion::async_recursion]
pub async fn process_symbol_recursive_enriched(
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
    parent_id: &str,
    symbol: HierarchicalSymbol,
    lsp_sync: i64,
    git_sync: i64,
    commit_hash: String,
) {
    let mut node = symbol.node;
    node.last_lsp_sync = Some(lsp_sync);
    node.last_git_sync = Some(git_sync);
    node.commit_hash = Some(commit_hash.clone());

    let node_id = node.id.clone();

    // NOTE: per-symbol reference matching deferred to resolve phase below
    // file_refs filtering happens there via (source_id, ref_loc) tuples

    nodes.push(node);
    edges.push(GraphEdge::new(EdgeType::Contains, parent_id.to_string(), node_id.clone()));

    for child in symbol.children {
        process_symbol_recursive_enriched(
            nodes,
            edges,
            &node_id,
            child,
            lsp_sync,
            git_sync,
            commit_hash.clone()
        ).await;
    }
}
