//! Codex CLI adapter.
//!
//! OpenAI's Codex uses `~/.codex/mcp.json` for MCP server configuration
//! in the same JSON shape as the other stdio-based agents. The same
//! adapter module covers any future config drift as long as the
//! `mcp_section` and `mcp_name` fields in `agents/manifest.toml` stay
//! accurate.

use super::{expand_home, server_for, AdapterError, AgentAdapter, AUTO_WORKSPACE, InstallScope};
use crate::cli::agents::manifest::AgentEntry;
use serde_json::{json, Value};
use std::path::Path;

pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn id(&self) -> &'static str { "codex" }

    fn install(&self, entry: &AgentEntry, scope: InstallScope) -> Result<(), AdapterError> {
        let Some(path) = (match scope {
            InstallScope::User => entry.config_user.as_str(),
            InstallScope::Project => entry.config_project.as_str(),
            InstallScope::Workspace => return Err(AdapterError::Unsupported(scope, self.id().into())),
        }).into() else { return Err(AdapterError::Unsupported(scope, self.id().into())); };
        let path = expand_home(path);
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
        let mut doc: Value = if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            serde_json::from_str(&raw).unwrap_or_else(|_| json!({}))
        } else { json!({}) };
        let workspace = AUTO_WORKSPACE.to_string();
        let server = server_for(entry, &workspace);
        let obj = doc.as_object_mut().ok_or_else(|| AdapterError::Shape("root not object".into()))?;
        let servers = obj.entry(entry.mcp_section.clone()).or_insert_with(|| json!({}));
        let servers_obj = servers.as_object_mut().ok_or_else(|| AdapterError::Shape("mcp section not object".into()))?;
        servers_obj.insert(entry.mcp_name.clone(), server);
        let serialized = serde_json::to_string_pretty(&doc)?;
        std::fs::write(&path, serialized)?;
        Ok(())
    }

    fn read(&self, entry: &AgentEntry, scope: InstallScope) -> Result<Value, AdapterError> {
        let Some(path) = (match scope {
            InstallScope::User => entry.config_user.as_str(),
            InstallScope::Project => entry.config_project.as_str(),
            InstallScope::Workspace => return Err(AdapterError::Unsupported(scope, self.id().into())),
        }).into() else { return Err(AdapterError::Unsupported(scope, self.id().into())); };
        let path: &Path = &expand_home(path);
        if !path.exists() { return Ok(Value::Null); }
        let raw = std::fs::read_to_string(path)?;
        let doc: Value = serde_json::from_str(&raw)?;
        Ok(doc.pointer(&format!("/{}", entry.mcp_section))
            .and_then(|s| s.get(&entry.mcp_name)).cloned()
            .unwrap_or(Value::Null))
    }
}
