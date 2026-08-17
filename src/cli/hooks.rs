//! `lain hooks claim|release` — thin CLI for agent pre-edit hooks.
//!
//! Reads (or creates) a session token from
//! `~/.config/lain/hooks/<agent_name>.session`, heartbeats, and proxies
//! the claim/release call to the lain server over HTTP MCP. Hook
//! scripts invoked by Claude / Cursor / Copilot etc. call this binary
//! to register the agent and claim files before editing.

use crate::config::hooks_dir;
use anyhow::{Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Subcommands for `lain hooks`.
#[derive(Debug, Subcommand)]
pub enum HooksAction {
    /// Claim a file (and optional symbol) for the agent's editing session.
    Claim {
        /// Lain server MCP URL (e.g. http://localhost:9999/mcp).
        #[arg(long)]
        url: String,
        /// Absolute file path being claimed.
        #[arg(long)]
        path: String,
        /// Optional symbol name within the file.
        #[arg(long, default_value = "")]
        symbol: String,
        /// Intent — `"edit"` or `"read"`.
        #[arg(long, default_value = "edit")]
        intent: String,
        /// Stable agent name (used as session file basename).
        #[arg(long, default_value = "lain-cli")]
        agent_name: String,
        /// Agent kind (`"claude"`, `"cursor"`, `"other"`, ...).
        #[arg(long, default_value = "other")]
        agent_kind: String,
    },
    /// Release a file the agent no longer needs.
    Release {
        /// Lain server MCP URL (e.g. http://localhost:9999/mcp).
        #[arg(long)]
        url: String,
        /// Absolute file path being released.
        #[arg(long)]
        path: String,
        /// Optional symbol name within the file (currently unused).
        #[arg(long, default_value = "")]
        symbol: String,
        /// Stable agent name (used as session file basename).
        #[arg(long, default_value = "lain-cli")]
        agent_name: String,
        /// Agent kind (`"claude"`, `"cursor"`, `"other"`, ...).
        #[arg(long, default_value = "other")]
        agent_kind: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct HookSession {
    agent_id: String,
    session_token: String,
    registered_at_unix: u64,
}

fn session_path(agent_name: &str) -> PathBuf {
    hooks_dir().join(format!("{agent_name}.session"))
}

fn read_session(agent_name: &str) -> Option<HookSession> {
    let path = session_path(agent_name);
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

fn write_session(agent_name: &str, sess: &HookSession) -> Result<()> {
    let path = session_path(agent_name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create hooks dir")?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(sess)?).context("write session")?;
    Ok(())
}

#[derive(Debug, Serialize)]
struct McpRequest {
    jsonrpc: &'static str,
    method: &'static str,
    params: serde_json::Value,
    id: u64,
}

#[derive(Debug, Deserialize)]
struct McpResponse {
    result: Option<McpResult>,
    error: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct McpResult {
    content: Vec<McpContent>,
    #[serde(default, rename = "isError")]
    is_error: bool,
}

#[derive(Debug, Deserialize)]
struct McpContent {
    text: String,
}

fn post_mcp(url: &str, method: &'static str, params: serde_json::Value) -> Result<McpResult> {
    let req = McpRequest {
        jsonrpc: "2.0",
        method,
        params,
        id: 1,
    };
    let client = reqwest::blocking::Client::new();
    let resp = client.post(url).json(&req).send().context("HTTP send")?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {} from lain server", resp.status());
    }
    let body: McpResponse = resp.json().context("parse JSON-RPC")?;
    if let Some(err) = body.error {
        anyhow::bail!("MCP error: {}", err);
    }
    body.result.context("no result in MCP response")
}

fn text_of(r: McpResult) -> Result<serde_json::Value> {
    let text = r
        .content
        .into_iter()
        .next()
        .map(|c| c.text)
        .context("empty result")?;
    serde_json::from_str(&text).context("parse result text")
}

fn register_if_needed(url: &str, name: &str, kind: &str) -> Result<HookSession> {
    if let Some(s) = read_session(name) {
        // Heartbeat. If the server has lost the session (e.g. it restarted
        // and `.lain` was cleared) the heartbeat returns `isError: true`
        // with non-JSON text like "heartbeat: unknown agent"; treat that
        // as a stale session and re-register instead of returning a dead
        // agent_id that claim/release will fail against.
        let stale = match post_mcp(
            url,
            "tools/call",
            serde_json::json!({
                "name": "heartbeat",
                "arguments": { "agent_id": s.agent_id, "session_token": s.session_token }
            }),
        ) {
            Ok(r) => r.is_error,
            Err(_) => true,
        };
        if !stale {
            return Ok(s);
        }
        // Drop stale session file before re-registering so we don't loop.
        let _ = std::fs::remove_file(session_path(name));
    }
    let pid = std::process::id();
    let result = post_mcp(
        url,
        "tools/call",
        serde_json::json!({
            "name": "register_agent",
            "arguments": { "name": name, "kind": kind, "pid": pid }
        }),
    )?;
    let text = text_of(result)?;
    let sess = HookSession {
        agent_id: text["agent_id"]
            .as_str()
            .context("no agent_id")?
            .to_string(),
        session_token: text["session_token"]
            .as_str()
            .context("no session_token")?
            .to_string(),
        registered_at_unix: chrono_now_unix(),
    };
    write_session(name, &sess)?;
    Ok(sess)
}

fn chrono_now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `lain hooks claim --url … --path … [--symbol …] [--intent …]`
pub fn claim(
    url: &str,
    path: &str,
    symbol: &str,
    intent: &str,
    agent_name: &str,
    agent_kind: &str,
) -> Result<()> {
    let sess = register_if_needed(url, agent_name, agent_kind)?;
    let mut files = serde_json::Map::new();
    files.insert("path".into(), serde_json::Value::String(path.to_string()));
    if !symbol.is_empty() {
        files.insert("symbols".into(), serde_json::json!([symbol]));
    }
    files.insert(
        "intent".into(),
        serde_json::Value::String(intent.to_string()),
    );
    let files_arr = serde_json::Value::Array(vec![serde_json::Value::Object(files)]);
    let args = serde_json::json!({
        "agent_id": sess.agent_id,
        "session_token": sess.session_token,
        "files": files_arr,
    });
    let result = post_mcp(
        url,
        "tools/call",
        serde_json::json!({
            "name": "claim_files",
            "arguments": args
        }),
    )?;
    let parsed = text_of(result)?;
    let granted = parsed["granted"].as_array().map(|a| a.len()).unwrap_or(0);
    let conflicts = parsed["conflicts"].as_array().map(|a| a.len()).unwrap_or(0);
    println!("lain hook: {granted} granted, {conflicts} conflict(s)");
    if conflicts > 0 {
        eprintln!("{}", serde_json::to_string(&parsed)?);
    }
    Ok(())
}

/// `lain hooks release --url … --path …`
pub fn release(
    url: &str,
    path: &str,
    _symbol: &str,
    agent_name: &str,
    agent_kind: &str,
) -> Result<()> {
    let sess = register_if_needed(url, agent_name, agent_kind)?;
    let args = serde_json::json!({
        "agent_id": sess.agent_id,
        "session_token": sess.session_token,
        "files": [{"path": path}],
    });
    let result = post_mcp(
        url,
        "tools/call",
        serde_json::json!({
            "name": "release_files",
            "arguments": args
        }),
    )?;
    let parsed = text_of(result)?;
    let released = parsed["released"].as_array().map(|a| a.len()).unwrap_or(0);
    println!("lain hook: released {released} file(s)");
    Ok(())
}
