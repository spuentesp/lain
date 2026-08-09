//! Per-agent config adapters.

use serde_json::{json, Value};

pub mod antigravity;
pub mod claude;
pub mod cline;
pub mod codex;
pub mod continue_dev;
pub mod cursor;
pub mod gemini;
pub mod kimi;
pub mod omp;

use crate::cmds::agents::manifest::AgentEntry;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallScope { User, Project, Workspace }

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("io: {0}")] Io(#[from] std::io::Error),
    #[error("serde: {0}")] Serde(#[from] serde_json::Error),
    #[error("toml: {0}")] Toml(#[from] toml::de::Error),
    #[error("config has unexpected shape: {0}")] Shape(String),
    #[error("adapter does not support {0:?} for {1}")] Unsupported(InstallScope, String),
}

pub trait AgentAdapter {
    fn id(&self) -> &'static str;
    fn install(&self, entry: &AgentEntry, scope: InstallScope) -> Result<(), AdapterError>;
    fn read(&self, entry: &AgentEntry, scope: InstallScope) -> Result<serde_json::Value, AdapterError>;
}

pub fn adapter_for(id: &str) -> Option<Box<dyn AgentAdapter>> {
    match id {
        "antigravity"  => Some(Box::new(antigravity::AntigravityAdapter)),
        "claude"        => Some(Box::new(claude::ClaudeAdapter)),
        "cline"         => Some(Box::new(cline::ClineAdapter)),
        "codex"         => Some(Box::new(codex::CodexAdapter)),
        "continue"      => Some(Box::new(continue_dev::ContinueAdapter)),
        "cursor"        => Some(Box::new(cursor::CursorAdapter)),
        "omp"           => Some(Box::new(omp::OmpAdapter)),
        "kimi"          => Some(Box::new(kimi::KimiAdapter)),
        _ => None,
    }
}

pub fn read_gemini_mcp_config(path: &Path) -> Result<Value, AdapterError> {
    if !path.exists() { return Ok(Value::Null); }
    let raw = std::fs::read_to_string(path)?;
    let doc: Value = serde_json::from_str(&raw)?;
    Ok(doc.get("mcpServers")
        .and_then(|s| s.get("lain"))
        .cloned()
        .unwrap_or(Value::Null))
}

pub fn write_gemini_mcp_config(path: &Path, server: &Value) -> Result<(), AdapterError> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let mut doc: Value = if path.exists() {
        let raw = std::fs::read_to_string(path)?;
        serde_json::from_str(&raw).unwrap_or_else(|_| json!({ "mcpServers": {} }))
    } else { json!({ "mcpServers": {} }) };
    let obj = doc.as_object_mut().ok_or_else(|| AdapterError::Shape("root not object".into()))?;
    let servers = obj.entry("mcpServers".to_string()).or_insert_with(|| json!({}));
    let servers_obj = servers.as_object_mut().ok_or_else(|| AdapterError::Shape("mcpServers not object".into()))?;
    servers_obj.insert("lain".to_string(), server.clone());
    let serialized = serde_json::to_string_pretty(&doc)?;
    std::fs::write(path, serialized)?;
    Ok(())
}

pub fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(p)
}

pub fn render_args(template: &[String], workspace: &str) -> Vec<String> {
    template.iter().map(|t| t.replace("{{workspace}}", workspace)).collect()
}

/// Build the per-agent MCP server JSON entry, branched on `entry.format`.
///
/// - `format = "http"`    -> `{ "url": "http://localhost:<LAIN_PORT>/mcp" }`
/// - `format = "sidecar"` -> `{ "command": "lain", "args": ["--mode", "sidecar", ...] }`
/// - otherwise (legacy)   -> `{ "command": <entry.command>, "args": <rendered default_args> }`
///
/// This is the single place that picks the install-time MCP server shape,
/// keeping every adapter's `install` method free of format branching.
pub fn server_for(entry: &AgentEntry, workspace: &str) -> Value {
    if entry.format == "http" {
        let port = std::env::var("LAIN_PORT").unwrap_or_else(|_| "9999".into());
        json!({ "url": format!("http://localhost:{}/mcp", port) })
    } else if entry.format == "sidecar" {
        let mut args = vec!["--mode".to_string(), "sidecar".to_string()];
        for a in render_args(&entry.default_args, workspace) {
            if a == "--transport" { continue; }
            args.push(a);
        }
        json!({ "command": "lain", "args": args })
    } else {
        let args = render_args(&entry.default_args, workspace);
        json!({ "command": entry.command, "args": args })
    }
}

#[allow(dead_code)]
pub fn resolve_target(entry: &AgentEntry, scope: InstallScope) -> Option<PathBuf> {
    let raw = match scope {
        InstallScope::User => entry.config_user.as_str(),
        InstallScope::Project => entry.config_project.as_str(),
        InstallScope::Workspace => return None,
    };
    if raw.is_empty() { None } else { Some(expand_home(raw)) }
}
