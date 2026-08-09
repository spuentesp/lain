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
        // Kimi only loads plugins that are listed in installed.json.
        register_kimi_plugin(&path)?;
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

fn register_kimi_plugin(plugin_path: &Path) -> Result<(), AdapterError> {
    // plugin_path is ~/.kimi-code/plugins/managed/lain/kimi.plugin.json
    // installed.json lives in ~/.kimi-code/plugins/installed.json
    let plugin_root = plugin_path
        .parent()
        .ok_or_else(|| AdapterError::Shape("kimi plugin path has no parent".into()))?;
    let plugins_dir = plugin_root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| AdapterError::Shape("kimi plugin path is too shallow".into()))?;
    let installed_path = plugins_dir.join("installed.json");
    let mut doc: Value = if installed_path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&installed_path)?)?
    } else {
        json!({ "plugins": [], "version": 1 })
    };
    let plugins = doc
        .get_mut("plugins")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| AdapterError::Shape("plugins not array".into()))?;
    let root = plugin_root.to_string_lossy().to_string();
    let existing = plugins.iter().position(|p| p.get("id").and_then(Value::as_str) == Some("lain"));
    let entry = json!({
        "id": "lain",
        "enabled": true,
        "source": "local-path",
        "originalSource": root,
        "root": root
    });
    match existing {
        Some(idx) => plugins[idx] = entry,
        None => plugins.push(entry),
    }
    std::fs::write(&installed_path, serde_json::to_string_pretty(&doc)?)?;
    Ok(())
}
