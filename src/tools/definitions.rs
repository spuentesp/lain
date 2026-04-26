//! Tool definitions and schemas
//!
//! Central registry of all MCP tools exposed by Lain.

use serde_json::Value;
use once_cell::sync::Lazy;
use std::sync::Arc;

/// Tool definition for MCP registration
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

impl ToolDefinition {
    /// Convert to rmcp::model::Tool
    pub fn to_rmcp_tool(&self) -> rmcp::model::Tool {
        use std::borrow::Cow;
        rmcp::model::Tool {
            name: Cow::Borrowed(self.name),
            description: Some(Cow::Borrowed(self.description)),
            input_schema: Arc::new(self.input_schema.as_object().unwrap().clone()),
            output_schema: None,
            annotations: None,
        }
    }
}

/// All tool definitions - single source of truth
pub static TOOL_DEFINITIONS: Lazy<Vec<ToolDefinition>> = Lazy::new(|| vec![
    ToolDefinition {
        name: "explore_architecture",
        description: "Get a high-level overview of code structure",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{"max_depth":{"type":"integer","description":"Only show files/modules up to this depth from entry points","default":2}},"required":[]}"#).unwrap(),
    },
    ToolDefinition {
        name: "trace_dependency",
        description: "Find all nodes that a symbol depends on",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{"symbol":{"type":"string","description":"The symbol name to trace"}},"required":["symbol"]}"#).unwrap(),
    },
    ToolDefinition {
        name: "semantic_search",
        description: "Find nodes by semantic meaning",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{"query":{"type":"string","description":"Natural language search query"},"limit":{"type":"integer","description":"Maximum results to return","default":10}},"required":["query"]}"#).unwrap(),
    },
    ToolDefinition {
        name: "get_blast_radius",
        description: "Find all code affected by changing a symbol",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{"symbol":{"type":"string","description":"The symbol to analyze"},"include_coupling":{"type":"boolean","description":"Include files that frequently co-change with this symbol's file","default":false}},"required":["symbol"]}"#).unwrap(),
    },
    ToolDefinition {
        name: "find_dead_code",
        description: "Identify symbols that are never called",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{},"required":[]}"#).unwrap(),
    },
    ToolDefinition {
        name: "get_coupling_radar",
        description: "Find files that are co-changed together based on git history",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{"symbol":{"type":"string","description":"The symbol to find coupling for"}},"required":["symbol"]}"#).unwrap(),
    },
    ToolDefinition {
        name: "find_anchors",
        description: "Find foundational building blocks (high fan-in, low fan-out)",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{"limit":{"type":"integer","description":"Maximum anchors to return","default":10}},"required":[]}"#).unwrap(),
    },
    ToolDefinition {
        name: "get_anchor_score",
        description: "Get the anchor score (importance) of a symbol",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{"symbol":{"type":"string","description":"The symbol to score"}},"required":["symbol"]}"#).unwrap(),
    },
    ToolDefinition {
        name: "get_context_depth",
        description: "Get how far a symbol is from entry points (main, App)",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{"symbol":{"type":"string","description":"The symbol to analyze"}},"required":["symbol"]}"#).unwrap(),
    },
    ToolDefinition {
        name: "run_enrichment",
        description: "Run the enrichment pipeline (calculate anchors + depths + co-changes)",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{},"required":[]}"#).unwrap(),
    },
    ToolDefinition {
        name: "sync_state",
        description: "Sync volatile overlay and refresh metrics from current git state",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{},"required":[]}"#).unwrap(),
    },
    ToolDefinition {
        name: "explain_symbol",
        description: "Get a comprehensive, context-rich summary of a symbol (depth, anchor score, co-changes)",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{"symbol":{"type":"string","description":"The symbol to explain"}},"required":["symbol"]}"#).unwrap(),
    },
    ToolDefinition {
        name: "list_entry_points",
        description: "List all project entry points (main, App, routes)",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{},"required":[]}"#).unwrap(),
    },
    ToolDefinition {
        name: "get_call_chain",
        description: "Find the shortest call path between two symbols",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{"from":{"type":"string","description":"Start symbol"},"to":{"type":"string","description":"End symbol"}},"required":["from","to"]}"#).unwrap(),
    },
    ToolDefinition {
        name: "navigate_to_anchor",
        description: "Find the foundational anchor node that this symbol eventually depends on",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{"symbol":{"type":"string","description":"The symbol to trace back"}},"required":["symbol"]}"#).unwrap(),
    },
    ToolDefinition {
        name: "compare_modules",
        description: "Compare two modules or files to find shared dependencies and interface differences",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{"module_a":{"type":"string","description":"First module/file path"},"module_b":{"type":"string","description":"Second module/file path"}},"required":["module_a","module_b"]}"#).unwrap(),
    },
    ToolDefinition {
        name: "suggest_refactor_targets",
        description: "Identify high-risk or high-debt areas for refactoring (e.g., high coupling, low anchor score)",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{"limit":{"type":"integer","description":"Maximum targets to suggest","default":5}},"required":[]}"#).unwrap(),
    },
    ToolDefinition {
        name: "get_health",
        description: "Get server health status and graph statistics",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{},"required":[]}"#).unwrap(),
    },
    ToolDefinition {
        name: "install_language_server",
        description: "Attempt to automatically install a missing language server (LSP) for a specific language extension",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{"language":{"type":"string","description":"The language extension (e.g., 'ts', 'go', 'rs', 'py') to install the LSP for"}},"required":["language"]}"#).unwrap(),
    },
    ToolDefinition {
        name: "get_layered_map",
        description: "Get a hierarchical map of the architecture starting from entry points, showing one layer at a time (N+1 approach)",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{"layer":{"type":"integer","description":"The layer depth to explore (0=entry points, 1=direct calls, etc.)","default":0},"granularity":{"type":"string","enum":["module","file","symbol"],"description":"Level of detail to return","default":"module"}},"required":[]}"#).unwrap(),
    },
    ToolDefinition {
        name: "get_master_map",
        description: "Get a high-level Staleness Report showing when each module was last synced from LSP and Git",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{},"required":[]}"#).unwrap(),
    },
    ToolDefinition {
        name: "get_agent_strategy",
        description: "Get the internal manual and strategy guide for AI agents on how to effectively use Lain's tools to reason about code",
        input_schema: serde_json::from_str(r#"{"type":"object","properties":{},"required":[]}"#).unwrap(),
    },
]);
