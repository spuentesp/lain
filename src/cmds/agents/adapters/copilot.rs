use super::{expand_home, AdapterError, AgentAdapter, InstallScope};
use crate::cmds::agents::manifest::AgentEntry;
use serde_json::{json, Value};
use std::path::Path;

/// Build the `servers.lain` JSON value for VS Code / Copilot.
///
/// Verified from the VS Code and GitHub Copilot MCP docs: a local
/// stdio MCP server is `servers.<name>.{ command, args }` where
/// `command` is a string and `args` is an array. This is distinct from
/// OpenCode's array-`command` shape. `command: "lain"` is a bare
/// PATH-resolvable name.
pub fn build_copilot_lain_entry(embedding_model: Option<&Path>) -> Value {
    let mut args: Vec<String> = vec![
        "--workspace".to_string(),
        "auto".to_string(),
        "--transport".to_string(),
        "stdio".to_string(),
    ];
    if let Some(model) = embedding_model {
        args.push("--embedding-model".to_string());
        args.push(model.to_string_lossy().to_string());
    }
    json!({
        "command": "lain",
        "args": args,
    })
}

pub struct CopilotAdapter;

impl AgentAdapter for CopilotAdapter {
    fn id(&self) -> &'static str { "copilot" }

    fn install(&self, entry: &AgentEntry, scope: InstallScope) -> Result<(), AdapterError> {
        let Some(raw_path) = (match scope {
            InstallScope::User => entry.config_user.as_str(),
            InstallScope::Project => entry.config_project.as_str(),
            InstallScope::Workspace => return Err(AdapterError::Unsupported(scope, self.id().into())),
        }).into() else {
            return Err(AdapterError::Unsupported(scope, self.id().into()));
        };
        let path = expand_home(raw_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut doc: Value = if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            serde_json::from_str(&raw).unwrap_or_else(|_| json!({}))
        } else {
            json!({})
        };
        let root = doc.as_object_mut()
            .ok_or_else(|| AdapterError::Shape("mcp.json root is not a JSON object".into()))?;
        let section = root.entry(entry.mcp_section.clone())
            .or_insert_with(|| json!({}));
        let section_obj = section.as_object_mut()
            .ok_or_else(|| AdapterError::Shape(format!("`{}` is not an object", entry.mcp_section)))?;
        // The adapter path doesn't have an embedding-model path; the init
        // path does. Without the model, Lain runs in stub embedder mode
        // (semantic search unavailable, every other tool works).
        section_obj.insert(entry.mcp_name.clone(), build_copilot_lain_entry(None));
        let serialized = serde_json::to_string_pretty(&doc)?;
        std::fs::write(&path, serialized)?;
        Ok(())
    }

    fn read(&self, entry: &AgentEntry, scope: InstallScope) -> Result<Value, AdapterError> {
        let Some(raw_path) = (match scope {
            InstallScope::User => entry.config_user.as_str(),
            InstallScope::Project => entry.config_project.as_str(),
            InstallScope::Workspace => return Err(AdapterError::Unsupported(scope, self.id().into())),
        }).into() else {
            return Err(AdapterError::Unsupported(scope, self.id().into()));
        };
        let path = expand_home(raw_path);
        if !path.exists() { return Ok(Value::Null); }
        let raw = std::fs::read_to_string(&path)?;
        let doc: Value = serde_json::from_str(&raw)?;
        Ok(doc.pointer(&format!("/{}", entry.mcp_section))
            .and_then(|s| s.get(&entry.mcp_name)).cloned()
            .unwrap_or(Value::Null))
    }

    fn remove(&self, entry: &AgentEntry, scope: InstallScope) -> Result<(), AdapterError> {
        let Some(raw_path) = (match scope {
            InstallScope::User => entry.config_user.as_str(),
            InstallScope::Project => entry.config_project.as_str(),
            InstallScope::Workspace => return Err(AdapterError::Unsupported(scope, self.id().into())),
        }).into() else {
            return Err(AdapterError::Unsupported(scope, self.id().into()));
        };
        let path = expand_home(raw_path);
        if !path.exists() { return Ok(()); }
        let raw = std::fs::read_to_string(&path)?;
        let mut doc: Value = serde_json::from_str(&raw)?;
        if let Some(section) = doc.get_mut(&entry.mcp_section).and_then(|v| v.as_object_mut()) {
            section.remove(&entry.mcp_name);
        }
        let serialized = serde_json::to_string_pretty(&doc)?;
        std::fs::write(&path, serialized)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmds::agents::manifest::AgentEntry;

    // Use the SHARED `HOME_LOCK` from `cmds::agents::tests`, promoted to
    // `pub` in the opencode fix wave. All HOME-mutating tests in the
    // agents test tree must acquire this same lock to avoid the
    // pre-existing race that surfaced during the opencode fix wave.
    pub use super::super::super::tests::HOME_LOCK;

    fn entry() -> AgentEntry {
        AgentEntry {
            id: "copilot".to_string(),
            display_name: "GitHub Copilot in VS Code".to_string(),
            binary: "code".to_string(),
            detect_paths: vec!["~/.config/Code".to_string(), "~/.vscode".to_string()],
            config_user: "~/.copilot/mcp-config.json".to_string(),
            config_project: ".vscode/mcp.json".to_string(),
            config_format: "jsonc".to_string(),
            mcp_section: "servers".to_string(),
            mcp_name: "lain".to_string(),
            transport: "stdio".to_string(),
            command: "lain".to_string(),
            default_args: vec![],
            headless_probe: vec!["code".to_string(), "--version".to_string()],
            format: "json".to_string(),
        }
    }

    #[test]
    fn copilot_adapter_install_read_remove_round_trip() {
        let _lock = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());
        let adapter = CopilotAdapter;
        let e = entry();
        adapter.install(&e, InstallScope::User).unwrap();
        let path = tmp.path().join(".copilot/mcp-config.json");
        assert!(path.exists(), "user-scope config not written");
        let body = std::fs::read_to_string(&path).unwrap();
        let doc: Value = serde_json::from_str(&body).unwrap();
        let lain = doc.pointer("/servers/lain").expect("servers.lain present");
        assert_eq!(lain.get("command").and_then(|v| v.as_str()), Some("lain"));
        let args = lain.get("args").and_then(|v| v.as_array()).expect("args is array");
        let cmd_strs: Vec<String> = args.iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert!(cmd_strs.windows(2).any(|w| w == ["--workspace", "auto"]));
        assert!(cmd_strs.windows(2).any(|w| w == ["--transport", "stdio"]));
        // Read returns the same shape.
        let read_back = adapter.read(&e, InstallScope::User).unwrap();
        assert_eq!(read_back, lain.clone());
    }

    #[test]
    fn copilot_adapter_preserves_other_servers() {
        let _lock = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());
        let path = tmp.path().join(".copilot/mcp-config.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "servers": {
                    "other-server": { "command": "x", "args": ["y"] }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let adapter = CopilotAdapter;
        let e = entry();
        adapter.install(&e, InstallScope::User).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let doc: Value = serde_json::from_str(&body).unwrap();
        assert!(
            doc.pointer("/servers/other-server").is_some(),
            "other-server must be preserved"
        );
        assert!(doc.pointer("/servers/lain").is_some(), "lain must be added");
    }

    #[test]
    fn copilot_adapter_remove_drops_only_lain() {
        let _lock = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());
        let adapter = CopilotAdapter;
        let e = entry();
        adapter.install(&e, InstallScope::User).unwrap();
        // Pre-seed with another server.
        let path = tmp.path().join(".copilot/mcp-config.json");
        let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        doc["servers"]["other-server"] = json!({ "command": "x", "args": ["y"] });
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

        adapter.remove(&e, InstallScope::User).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let doc: Value = serde_json::from_str(&body).unwrap();
        assert!(doc.pointer("/servers/lain").is_none(), "lain must be removed");
        assert!(doc.pointer("/servers/other-server").is_some(), "other-server preserved");
    }
}
