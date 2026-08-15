//! Tool definitions and schemas
//!
//! `ToolDefinition` struct used by `ToolRegistry::definitions()` to build MCP schema.

use serde_json::Value;

/// Tool definition for MCP registration
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}
