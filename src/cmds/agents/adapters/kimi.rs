use super::{expand_home, server_for, AdapterError, AgentAdapter, InstallScope};
use crate::cmds::agents::manifest::AgentEntry;
use serde_json::{json, Value};
use std::path::Path;

pub struct KimiAdapter;

impl AgentAdapter for KimiAdapter {
    fn id(&self) -> &'static str { "kimi" }

    fn install(&self, entry: &AgentEntry, scope: InstallScope) -> Result<(), AdapterError> {
        let Some(path) = (match scope {
            InstallScope::User => entry.config_user.as_str(),
            InstallScope::Project => entry.config_project.as_str(),
            InstallScope::Workspace => return Err(AdapterError::Unsupported(scope, self.id().into())),
        }).into() else { return Err(AdapterError::Unsupported(scope, self.id().into())); };
        let path = expand_home(path);
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
        let workspace = std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let server = server_for(entry, &workspace);
        let doc = json!({
            "name": "lain",
            "version": "0.4.2",
            "mcpServers": { "lain": server }
        });
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
        std::fs::write(&path, serde_json::to_string_pretty(&doc)?)?;
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
        Ok(doc.pointer("/mcpServers/lain").cloned().unwrap_or(Value::Null))
    }
}
