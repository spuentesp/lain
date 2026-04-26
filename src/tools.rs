//! Modular tool execution system
//!
//! Follows SOLID and DRY principles by delegating logic to specialized handlers.

pub mod definitions;
pub mod utils;
pub mod handlers;

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::git::GitSensor;
use crate::lsp::LspMultiplexer;
use crate::nlp::NlpEmbedder;
use crate::overlay::VolatileOverlay;
use serde_json::{Map, Value};
use std::sync::Arc;
use parking_lot::Mutex;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{info, error};

pub use definitions::{ToolDefinition, TOOL_DEFINITIONS};
use utils::*;

/// Tool executor - orchestrates MCP tool execution by delegating to specialized handlers.
#[derive(Clone)]
pub struct ToolExecutor {
    graph: GraphDatabase,
    _overlay: VolatileOverlay,
    embedder: NlpEmbedder,
    git: Arc<Mutex<GitSensor>>,
    lsp: Arc<AsyncMutex<LspMultiplexer>>,
}

impl ToolExecutor {
    pub fn new(
        graph: GraphDatabase,
        overlay: VolatileOverlay,
        embedder: NlpEmbedder,
        git: Arc<Mutex<GitSensor>>,
        lsp: Arc<AsyncMutex<LspMultiplexer>>,
    ) -> Self {
        Self {
            graph,
            _overlay: overlay,
            embedder,
            git,
            lsp,
        }
    }

    /// Primary dispatcher for all MCP tools
    pub async fn call(&self, name: &str, arguments: Option<&Map<String, Value>>) -> Result<String, LainError> {
        match name {
            // Architecture
            "explore_architecture" => {
                let max_depth = get_usize_arg(arguments, "max_depth").unwrap_or(2);
                handlers::architecture::explore_architecture(&self.graph, max_depth)
            },
            "list_entry_points" => handlers::architecture::list_entry_points(&self.graph),
            "compare_modules" => {
                let a = get_str_arg(arguments, "module_a");
                let b = get_str_arg(arguments, "module_b");
                handlers::architecture::compare_modules(&self.graph, a, b)
            }

            // Navigation
            "trace_dependency" => {
                let symbol = get_str_arg(arguments, "symbol");
                self.ingest_references(symbol).await?;
                handlers::navigation::trace_dependency(&self.graph, symbol)
            }
            "get_call_chain" => {
                let from = get_str_arg(arguments, "from");
                let to = get_str_arg(arguments, "to");
                self.ingest_references(from).await?;
                self.ingest_references(to).await?;
                handlers::navigation::get_call_chain(&self.graph, from, to)
            }
            "navigate_to_anchor" => {
                let symbol = get_str_arg(arguments, "symbol");
                handlers::navigation::navigate_to_anchor(&self.graph, symbol)
            }
            "get_layered_map" => {
                let layer = get_usize_arg(arguments, "layer").unwrap_or(0);
                let granularity = get_str_arg(arguments, "granularity");
                handlers::navigation::get_layered_map(&self.graph, layer, granularity)
            }
            "get_master_map" => handlers::architecture::get_master_map(&self.graph),

            // Search
            "semantic_search" => {
                let query = get_str_arg(arguments, "query");
                let limit = get_usize_arg(arguments, "limit").unwrap_or(10);
                handlers::search::semantic_search(&self.graph, &self.embedder, query, limit)
            }

            // Impact
            "get_blast_radius" => {
                let symbol = get_str_arg(arguments, "symbol");
                let include_coupling = get_bool_arg(arguments, "include_coupling").unwrap_or(false);
                self.ingest_references(symbol).await?;
                handlers::impact::get_blast_radius(&self.graph, symbol, include_coupling)
            }
            "get_coupling_radar" => {
                let symbol = get_str_arg(arguments, "symbol");
                handlers::impact::get_coupling_radar(&self.graph, symbol)
            }

            // Metrics & Explanation
            "find_anchors" => {
                let limit = get_usize_arg(arguments, "limit").unwrap_or(10);
                handlers::metrics::find_anchors(&self.graph, limit)
            }
            "get_anchor_score" => {
                let symbol = get_str_arg(arguments, "symbol");
                handlers::metrics::get_anchor_score(&self.graph, symbol)
            }
            "get_context_depth" => {
                let symbol = get_str_arg(arguments, "symbol");
                handlers::metrics::get_context_depth(&self.graph, symbol)
            }
            "find_dead_code" => handlers::metrics::find_dead_code(&self.graph),
            "explain_symbol" => {
                let symbol = get_str_arg(arguments, "symbol");
                handlers::metrics::explain_symbol(&self.graph, symbol)
            }
            "suggest_refactor_targets" => {
                let limit = get_usize_arg(arguments, "limit").unwrap_or(5);
                handlers::metrics::suggest_refactor_targets(&self.graph, limit)
            }

            // System
            "get_health" => self.get_health().await,
            "install_language_server" => {
                let language = get_str_arg(arguments, "language");
                self.install_language_server(language).await
            }
            "get_agent_strategy" => self.get_agent_strategy(),

            // System
            "run_enrichment" => handlers::enrichment::run_enrichment(&self.graph, &self.git),
            "sync_state" => handlers::enrichment::sync_state(&self.graph, &self.git),

            _ => Err(LainError::NotFound(format!("Unknown tool: {}", name))),
        }
    }

    async fn get_health(&self) -> Result<String, LainError> {
        let (nodes, edges) = self.graph.get_stats();
        let last_commit = self.graph.get_last_commit()?.unwrap_or_else(|| "None".to_string());

        let mut output = format!(
            "## Lain Server Health\n\n- **Status:** Operational ✅\n- **Nodes:** {}\n- **Edges:** {}\n- **Last Enriched Commit:** {}\n- **NLP Model:** Loaded (all-MiniLM-L6-v2)\n",
            nodes, edges, last_commit
        );

        // Add language support status
        output.push_str("\n### Language Support\n");
        let langs = {
            let lsp_guard = self.lsp.lock().await;
            lsp_guard.get_supported_languages()
        };
        
        let mut seen_binaries = std::collections::HashSet::new();
        for (_, binary, available) in langs {
            if seen_binaries.contains(&binary) { continue; }
            seen_binaries.insert(binary.clone());
            
            let status = if available { "✅" } else { "❌ (Missing)" };
            output.push_str(&format!("- **{}**: {}\n", binary, status));
        }

        Ok(output)
    }

    async fn install_language_server(&self, language: &str) -> Result<String, LainError> {
        info!("Requesting installation of LSP for: {}", language);
        
        let lsp_clone = Arc::clone(&self.lsp);
        let lang = language.to_string();
        
        tokio::spawn(async move {
            let mut lsp_guard = lsp_clone.lock().await;
            if let Err(e) = lsp_guard.install_server(&lang).await {
                error!("LSP installation for {} failed: {}", lang, e);
            }
        });

        Ok(format!("Installation of LSP for '{}' started in background. Check 'get_health' in a minute to see if it's available.", language))
    }

    fn get_agent_strategy(&self) -> Result<String, LainError> {
        Ok(r#"# AI Agent Strategy Guide for Lain

If you are an agent, follow this flow to build a high-fidelity mental model of the codebase:

1. **System Health**: Call `get_health` to verify language servers are active.
2. **Knowledge Freshness**: Call `get_master_map` to see if you need to run `sync_state`.
3. **Global Context**: Call `get_layered_map(layer: 0, granularity: "module")` to see root modules.
4. **Architectural Anchors**: Call `find_anchors` to find the most foundational/stable parts of the code.
5. **Impact Analysis**: Use `get_blast_radius` before making changes to see downstream ripple effects.
6. **Intent Discovery**: Use `semantic_search` if you know *what* you want to find but not *where* it is.

*Use these tools incrementally (N+1 approach) to avoid context window overflow.*
"#.to_string())
    }

    /// On-demand ingestion of references for a specific symbol
    async fn ingest_references(&self, symbol_name: &str) -> Result<(), LainError> {
        let node = self.graph.get_node(symbol_name)?;
        let Some(target_node) = node else { return Ok(()); };

        if self.graph.has_references_from(&target_node.id) {
            return Ok(());
        }

        info!("Ingesting references for '{}' on-demand", symbol_name);
        
        let refs = {
            let mut lsp = self.lsp.lock().await;
            lsp.get_references(
                std::path::Path::new(&target_node.path),
                target_node.line_start.unwrap_or(0),
                0 // Column 0 is usually enough for definitions
            ).await?
        };

        for r in refs {
            if let Some(source_node) = self.graph.get_node_at_location(&r.path.to_string_lossy(), r.line) {
                if source_node.id != target_node.id {
                    let edge = crate::schema::GraphEdge::new(
                        crate::schema::EdgeType::Calls,
                        source_node.id.clone(),
                        target_node.id.clone()
                    );
                    self.graph.insert_edge(&edge)?;
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{GraphNode, NodeType};

    fn create_test_executor_with_graph(graph: GraphDatabase) -> ToolExecutor {
        let overlay = VolatileOverlay::new();
        let embedder = NlpEmbedder::new().unwrap();
        let git = Arc::new(Mutex::new(GitSensor::new(std::path::Path::new(".")).unwrap()));
        let lsp = Arc::new(AsyncMutex::new(LspMultiplexer::new(std::path::Path::new(".")).unwrap()));
        ToolExecutor::new(graph, overlay, embedder, git, lsp)
    }

    fn create_test_executor() -> ToolExecutor {
        let temp_dir = std::env::temp_dir().join("lain_test_graph");
        let graph = GraphDatabase::new(&temp_dir).unwrap();
        create_test_executor_with_graph(graph)
    }

    fn create_executor_with_node(node: GraphNode) -> ToolExecutor {
        let temp_dir = std::env::temp_dir().join("lain_test_graph");
        let graph = GraphDatabase::new(&temp_dir).unwrap();
        graph.insert_node(&node).unwrap();
        create_test_executor_with_graph(graph)
    }

    #[test]
    fn test_tool_definitions_count() {
        let tools = TOOL_DEFINITIONS.iter().collect::<Vec<_>>();
        assert_eq!(tools.len(), 22, "Expected 22 tool definitions");
    }

    #[test]
    fn test_tool_definitions_have_valid_names() {
        for tool in TOOL_DEFINITIONS.iter() {
            assert!(!tool.name.is_empty(), "Tool name should not be empty");
            assert!(tool.name.chars().all(|c| c.is_lowercase() || c == '_'),
                "Tool name '{}' should be lowercase with underscores", tool.name);
        }
    }

    #[test]
    fn test_tool_definitions_have_valid_schemas() {
        for tool in TOOL_DEFINITIONS.iter() {
            assert!(tool.input_schema.is_object(), "Schema should be a JSON object");
            let obj = tool.input_schema.as_object().unwrap();
            assert!(obj.contains_key("type"), "Schema should have 'type' field");
            assert!(obj.contains_key("properties"), "Schema should have 'properties' field");
            assert!(obj.contains_key("required"), "Schema should have 'required' field");
        }
    }

    #[test]
    fn test_tool_definitions_specific_tools() {
        let tool_names: Vec<String> = TOOL_DEFINITIONS.iter().map(|t| t.name.to_string()).collect();
        let expected = vec![
            "explore_architecture",
            "trace_dependency",
            "semantic_search",
            "get_blast_radius",
            "find_dead_code",
            "get_coupling_radar",
            "find_anchors",
            "get_anchor_score",
            "get_context_depth",
            "run_enrichment",
            "sync_state",
            "explain_symbol",
            "list_entry_points",
            "get_call_chain",
            "navigate_to_anchor",
            "compare_modules",
            "suggest_refactor_targets",
            "get_health",
            "install_language_server",
            "get_layered_map",
            "get_master_map",
            "get_agent_strategy",
        ];
        for name in expected {
            assert!(tool_names.contains(&name.to_string()), "Missing tool: {}", name);
        }
    }

    #[tokio::test]
    async fn test_tool_executor_call_unknown_tool() {
        let executor = create_test_executor();
        let result = executor.call("nonexistent_tool", None).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Unknown tool"), "Error should mention unknown tool");
    }

    #[tokio::test]
    async fn test_explore_architecture_empty_graph() {
        let executor = create_test_executor();
        let result = executor.call("explore_architecture", None).await;
        assert!(result.is_ok(), "explore_architecture should succeed");
        let output = result.unwrap();
        assert!(output.contains("Architecture Overview"), "Output should contain header");
        assert!(output.contains("Found 0 total files"), "Empty graph should show 0 files");
    }

    #[tokio::test]
    async fn test_explore_architecture_with_files() {
        let mut node = GraphNode::new(NodeType::File, "test.rs".to_string(), "/test.rs".to_string());
        node.depth_from_main = Some(0); // Ensure it's not filtered by max_depth
        let executor = create_executor_with_node(node);

        let result = executor.call("explore_architecture", None).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Found 1 total files"));
        assert!(output.contains("test.rs"));
    }

    #[tokio::test]
    async fn test_find_dead_code_empty() {
        let executor = create_test_executor();
        let result = executor.call("find_dead_code", None).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Found 0 dead code symbols"));
    }
}
