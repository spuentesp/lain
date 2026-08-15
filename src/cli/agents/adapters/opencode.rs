use super::{expand_home, AdapterError, AgentAdapter, InstallScope};
use crate::cli::agents::manifest::AgentEntry;
use serde_json::{json, Value};
use std::path::Path;

/// Build the `mcp.lain` JSON value for OpenCode's `opencode.json`.
///
/// Verified against the schema at <https://opencode.ai/docs/mcp-servers>:
/// `command` is an **Array** `[executable, arg1, arg2, ...]` — a
/// string `command` is invalid. We always set `type: "local"`,
/// `enabled: true`, and `timeout: 30000` (the default 5000ms is too
/// short for Lain's cold-start NLP model load).
pub fn build_opencode_lain_entry(embedding_model: Option<&Path>) -> Value {
    let mut command: Vec<String> = vec![
        "lain".to_string(),
        "--workspace".to_string(),
        "auto".to_string(),
        "--transport".to_string(),
        "stdio".to_string(),
    ];
    if let Some(model) = embedding_model {
        command.push("--embedding-model".to_string());
        command.push(model.to_string_lossy().to_string());
    }
    json!({
        "type": "local",
        "command": command,
        "enabled": true,
        "timeout": 30000
    })
}

pub struct OpenCodeAdapter;

impl AgentAdapter for OpenCodeAdapter {
    fn id(&self) -> &'static str { "opencode" }

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
            .ok_or_else(|| AdapterError::Shape("opencode.json root is not a JSON object".into()))?;
        let mcp = root.entry(entry.mcp_section.clone()).or_insert_with(|| json!({}));
        let mcp_obj = mcp.as_object_mut()
            .ok_or_else(|| AdapterError::Shape("opencode.json `mcp` is not an object".into()))?;
        // The adapter path doesn't have an embedding-model path; the init
        // path does. Without the model, Lain runs in stub embedder mode
        // (semantic search unavailable, every other tool works).
        mcp_obj.insert(entry.mcp_name.clone(), build_opencode_lain_entry(None));
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
        if let Some(mcp) = doc.get_mut(&entry.mcp_section).and_then(|v| v.as_object_mut()) {
            mcp.remove(&entry.mcp_name);
        }
        let serialized = serde_json::to_string_pretty(&doc)?;
        std::fs::write(&path, serialized)?;
        Ok(())
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::cli::agents::manifest::AgentEntry;

    // Serializes tests that mutate the process-global HOME env var so they
    // don't race each other when cargo runs the suite in parallel. Aliased to
    // the agents-level `HOME_LOCK` so all HOME-mutating tests across the
    // binary share one mutex (the previous per-module mutexes did not
    // synchronize with each other, producing intermittent failures between
    // `claude_round_trip_under_temp_home` and the opencode adapter tests).
    pub use super::super::super::tests::HOME_LOCK;

    fn entry() -> AgentEntry {
        // Minimal manifest row. Only the fields the adapter reads matter here.
        AgentEntry {
            id: "opencode".to_string(),
            display_name: "OpenCode".to_string(),
            binary: "opencode".to_string(),
            detect_paths: vec!["~/.config/opencode".to_string()],
            config_user: "~/.config/opencode/opencode.json".to_string(),
            config_project: "opencode.json".to_string(),
            config_format: "jsonc".to_string(),
            mcp_section: "mcp".to_string(),
            mcp_name: "lain".to_string(),
            transport: "stdio".to_string(),
            command: "lain".to_string(),
            default_args: vec![],
            headless_probe: vec!["opencode".to_string(), "--version".to_string()],
            format: "json".to_string(),
        }
    }

    #[test]
    fn opencode_adapter_install_read_remove_round_trip() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        // Redirect HOME so the user-scope path lives inside the tempdir.
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", tmp.path());
        let adapter = OpenCodeAdapter;
        let e = entry();
        adapter.install(&e, InstallScope::User).unwrap();
        let path = tmp.path().join(".config/opencode/opencode.json");
        assert!(path.exists(), "config file not written");
        let body = std::fs::read_to_string(&path).unwrap();
        let doc: Value = serde_json::from_str(&body).unwrap();
        let lain = doc.pointer("/mcp/lain").expect("mcp.lain present");
        assert_eq!(lain.get("type").and_then(|v| v.as_str()), Some("local"));
        let cmd = lain.get("command").and_then(|v| v.as_array()).expect("command is array");
        let cmd_strs: Vec<String> = cmd.iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert_eq!(cmd_strs.first().map(String::as_str), Some("lain"));
        assert!(cmd_strs.windows(2).any(|w| w == ["--workspace", "auto"]));
        assert!(cmd_strs.windows(2).any(|w| w == ["--transport", "stdio"]));
        assert_eq!(lain.get("enabled"), Some(&Value::Bool(true)));
        assert_eq!(lain.get("timeout"), Some(&json!(30000)));

        // Read returns the same shape.
        let read_back = adapter.read(&e, InstallScope::User).unwrap();
        assert_eq!(read_back, lain.clone());

        if let Some(h) = original_home { std::env::set_var("HOME", h); }
    }

    #[test]
    fn opencode_adapter_preserves_other_mcp_servers() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", tmp.path());
        let path = tmp.path().join(".config/opencode/opencode.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "mcp": {
                    "other-server": { "type": "local", "command": ["x"], "enabled": true }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let adapter = OpenCodeAdapter;
        let e = entry();
        adapter.install(&e, InstallScope::User).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let doc: Value = serde_json::from_str(&body).unwrap();
        assert!(
            doc.pointer("/mcp/other-server").is_some(),
            "other-server must be preserved"
        );
        assert!(doc.pointer("/mcp/lain").is_some(), "lain must be added");
        if let Some(h) = original_home { std::env::set_var("HOME", h); }
    }

    #[test]
    fn opencode_adapter_remove_drops_only_lain() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", tmp.path());
        let adapter = OpenCodeAdapter;
        let e = entry();
        adapter.install(&e, InstallScope::User).unwrap();
        // Pre-seed with another server so we can assert remove preserves it.
        let path = tmp.path().join(".config/opencode/opencode.json");
        let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        doc["mcp"]["other-server"] = json!({ "type": "local", "command": ["x"], "enabled": true });
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

        adapter.remove(&e, InstallScope::User).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let doc: Value = serde_json::from_str(&body).unwrap();
        assert!(doc.pointer("/mcp/lain").is_none(), "lain must be removed");
        assert!(doc.pointer("/mcp/other-server").is_some(), "other-server preserved");
        if let Some(h) = original_home { std::env::set_var("HOME", h); }
    }
}
