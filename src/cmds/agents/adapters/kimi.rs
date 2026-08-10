use super::{expand_home, AdapterError, AgentAdapter, AUTO_WORKSPACE, InstallScope};
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
        let workspace = AUTO_WORKSPACE.to_string();

        // Kimi's plugin security model only allows stdio MCP commands that are
        // either on PATH or a `./` path inside the plugin root, and `cwd` must
        // also be `./` and inside the plugin root. An absolute command path is
        // silently ignored. Build a wrapper script inside the plugin root that
        // execs the real binary, then reference it with `./bin/lain`.
        let plugin_root = path
            .parent()
            .ok_or_else(|| AdapterError::Shape("kimi plugin path has no parent".into()))?;
        let wrapper_dir = plugin_root.join("bin");
        std::fs::create_dir_all(&wrapper_dir)?;
        let wrapper = wrapper_dir.join("lain");
        let wrapper_script = format!(
            "#!/usr/bin/env bash\n# Kimi plugin wrapper for Lain. Re-execs the real binary so\n# the plugin manifest can use a `./` relative command.\nexec \"{}\" \"$@\"\n",
            entry.command
        );
        std::fs::write(&wrapper, wrapper_script)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;
        }

        // Build the server entry with plugin-root-relative command/cwd. Args
        // can be absolute paths (workspace, model) because they are not subject
        // to the plugin-root restriction.
        let args = super::render_args(&entry.default_args, &workspace);
        let server = json!({
            "command": "./bin/lain",
            "args": args,
            "cwd": "./"
        });

        let doc = json!({
            "name": "lain",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Local codebase memory and query tools for Lain.",
            "keywords": ["codebase", "graph", "mcp", "lain"],
            "mcpServers": { "lain": server },
            "interface": {
                "displayName": "Lain",
                "shortDescription": "Local codebase memory and query tools",
                "developerName": "Lain"
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&doc)?)?;

        // Kimi appears to load plugin MCP servers more reliably when the
        // plugin root also contains a skill file. Copy the bundled skill if
        // it exists next to the manifest.
        let skills_dir = plugin_root.join("skills").join("lain");
        std::fs::create_dir_all(&skills_dir)?;
        let skill_src = std::path::Path::new("hooks/kimi/skills/lain/SKILL.md");
        if skill_src.exists() {
            std::fs::copy(skill_src, skills_dir.join("SKILL.md"))?;
        }

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

    fn remove(&self, entry: &AgentEntry, scope: InstallScope) -> Result<(), AdapterError> {
        let Some(path) = (match scope {
            InstallScope::User => entry.config_user.as_str(),
            InstallScope::Project => entry.config_project.as_str(),
            InstallScope::Workspace => return Err(AdapterError::Unsupported(scope, self.id().into())),
        }).into() else { return Err(AdapterError::Unsupported(scope, self.id().into())); };
        let path = expand_home(path);
        unregister_kimi_plugin(&path)
    }
}

fn plugin_root_and_installed_path(plugin_path: &Path) -> Result<(std::path::PathBuf, std::path::PathBuf), AdapterError> {
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
    Ok((plugin_root.to_path_buf(), installed_path))
}

fn register_kimi_plugin(plugin_path: &Path) -> Result<(), AdapterError> {
    let (plugin_root, installed_path) = plugin_root_and_installed_path(plugin_path)?;
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

/// Remove the Lain plugin registration from Kimi's `installed.json` and
/// delete the managed plugin directory. Called from the generic remove path
/// when it detects a Kimi adapter.
pub fn unregister_kimi_plugin(plugin_path: &Path) -> Result<(), AdapterError> {
    let (plugin_root, installed_path) = plugin_root_and_installed_path(plugin_path)?;
    if installed_path.exists() {
        let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&installed_path)?)?;
        if let Some(plugins) = doc.get_mut("plugins").and_then(Value::as_array_mut) {
            plugins.retain(|p| p.get("id").and_then(Value::as_str) != Some("lain"));
        }
        std::fs::write(&installed_path, serde_json::to_string_pretty(&doc)?)?;
    }
    if plugin_root.exists() {
        let _ = std::fs::remove_dir_all(&plugin_root);
    }
    Ok(())
}
