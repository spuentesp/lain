//! MCP ServerHandler implementation
//!
//! This module contains only the MCP protocol glue.
//! All tool logic is delegated to ToolExecutor.

use crate::tools::{ToolExecutor, TOOL_DEFINITIONS};
use rmcp::model::{CallToolRequestParam, Content, ErrorData};
use rmcp::service::{RequestContext, RoleServer};

/// MCP handler - thin protocol dispatcher
#[derive(Clone)]
pub struct McpHandler {
    executor: ToolExecutor,
}

impl McpHandler {
    pub fn new(executor: ToolExecutor) -> Self {
        Self { executor }
    }

    /// Get the underlying executor for testing
    #[cfg(test)]
    pub fn executor(&self) -> &ToolExecutor {
        &self.executor
    }
}

impl rmcp::ServerHandler for McpHandler {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::InitializeResult {
            protocol_version: rmcp::model::ProtocolVersion::LATEST,
            capabilities: rmcp::model::ServerCapabilities {
                tools: Some(rmcp::model::ToolsCapability {
                    list_changed: Some(false),
                }),
                logging: None,
                prompts: None,
                resources: None,
                completions: None,
                experimental: None,
            },
            server_info: rmcp::model::Implementation {
                name: "lain-mcp".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            instructions: None,
        }
    }

    fn initialize(
        &self,
        _request: rmcp::model::InitializeRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<rmcp::model::InitializeResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(self.get_info()))
    }

    fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<rmcp::model::ListToolsResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(rmcp::model::ListToolsResult::with_all_items(
            TOOL_DEFINITIONS.iter().map(|t| t.to_rmcp_tool()).collect()
        )))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<rmcp::model::CallToolResult, ErrorData>> + Send + '_ {
        let name = request.name.as_ref().to_string();
        let arguments = request.arguments.clone();
        let executor = self.executor.clone();

        async move {
            let result = executor.call(&name, arguments.as_ref()).await;
            match result {
                Ok(output) => Ok(rmcp::model::CallToolResult::success(vec![Content::text(output)])),
                Err(e) => Ok(rmcp::model::CallToolResult::error(vec![Content::text(format!("Error: {}", e))])),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::GraphDatabase;
    use crate::nlp::NlpEmbedder;
    use crate::overlay::VolatileOverlay;
    use crate::lsp::LspMultiplexer;
    use std::sync::Arc;
    use parking_lot::Mutex;
    use crate::git::GitSensor;
    use rmcp::ServerHandler;

    fn create_test_handler() -> McpHandler {
        let temp_dir = std::env::temp_dir().join("lain_test_graph");
        let graph = GraphDatabase::new(&temp_dir).unwrap();
        let overlay = VolatileOverlay::new();
        let embedder = NlpEmbedder::new().unwrap();
        let git = Arc::new(Mutex::new(GitSensor::new(std::path::Path::new(".")).unwrap()));
        let lsp = Arc::new(tokio::sync::Mutex::new(LspMultiplexer::new(&temp_dir).unwrap()));
        let executor = ToolExecutor::new(graph, overlay, embedder, git, lsp);
        McpHandler::new(executor)
    }

    #[test]
    fn test_mcp_handler_new() {
        let handler = create_test_handler();
        // Just verify handler was created successfully
        let _ = handler.executor();
    }

    #[test]
    fn test_get_info_has_correct_name() {
        let handler = create_test_handler();
        let info = handler.get_info();
        assert_eq!(info.server_info.name, "lain-mcp");
    }

    #[test]
    fn test_get_info_has_tools_capability() {
        let handler = create_test_handler();
        let info = handler.get_info();
        assert!(info.capabilities.tools.is_some());
    }

    #[test]
    fn test_get_info_version_matches_cargo() {
        let handler = create_test_handler();
        let info = handler.get_info();
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn test_tool_definitions_converted_to_rmcp_tools() {
        let tools: Vec<_> = TOOL_DEFINITIONS.iter().map(|t| t.to_rmcp_tool()).collect();
        assert_eq!(tools.len(), 22);
        assert!(tools.iter().all(|t| !t.name.is_empty()));
    }
}