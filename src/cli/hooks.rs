//! `lain hooks claim|release` — thin CLI for agent pre-edit hooks.
//!
//! Reads (or creates) a session token from
//! `~/.config/lain/hooks/<agent_name>.session`, heartbeats, and proxies
//! the claim/release call to the lain server over HTTP MCP. Hook
//! scripts invoked by Claude / Cursor / Copilot etc. call this binary
//! to register the agent and claim files before editing.

use crate::config::hooks_dir;
use crate::server::presence::{AgentId, AgentKind, ClaimIntent};
use crate::server::presence_lock;
use anyhow::{Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

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
        /// Optional parent session id — used by subagents to declare the
        /// parent agent that spawned them. Empty string means "no parent".
        #[arg(long, default_value = "")]
        parent_session_id: String,
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
        /// Optional parent session id (mirrors `claim` for symmetry).
        #[arg(long, default_value = "")]
        parent_session_id: String,
    },
    /// Detect symbol-level overlap between two git refs in a federation
    /// workspace. Used by the pre-commit hook to refuse a commit that
    /// would touch symbols also touched by `--base`.
    OverlapCheck {
        /// Lain server MCP URL (e.g. http://localhost:9999/mcp).
        #[arg(long)]
        url: String,
        /// Base ref — commit SHA, branch name, or `HEAD~N`. Resolved
        /// to a full SHA before being sent to the server.
        #[arg(long)]
        base: String,
        /// Head ref carrying the local work. Defaults to `HEAD` when
        /// omitted (matches the MCP tool's own default).
        #[arg(long)]
        head: Option<String>,
        /// Federation workspace name to scan for overlap.
        #[arg(long)]
        workspace: String,
    },
    /// Acquire a filesystem lock sentinel for `path` under
    /// `workspace_root`. Best-effort; conflict does not roll back —
    /// the conflict holder is printed to stderr and the process exits
    /// non-zero so the caller can decide whether to continue.
    Lock {
        /// Workspace root (the directory containing `.lain/`).
        #[arg(long)]
        workspace_root: String,
        /// Absolute path of the file being claimed.
        #[arg(long)]
        path: String,
        /// Stable agent name (used to derive a deterministic `AgentId`).
        #[arg(long)]
        agent_name: String,
        /// Agent kind (`"claude-code"`, `"kimi"`, `"other"`, ...).
        #[arg(long, default_value = "other")]
        agent_kind: String,
        /// Intent — `"edit"` or `"read"`.
        #[arg(long, default_value = "edit")]
        intent: String,
        /// Advisory TTL in seconds. `presence_lock::try_lock` uses a
        /// 5s TTL internally; this flag is accepted so the pre-commit
        /// hook and other callers can forward it, but is not yet
        /// plumbed through to the lock layer.
        #[arg(long)]
        ttl_seconds: Option<u64>,
    },
    /// Remove the filesystem lock sentinel for `path` under
    /// `workspace_root`. Idempotent — ENOENT is treated as success.
    Unlock {
        /// Workspace root (the directory containing `.lain/`).
        #[arg(long)]
        workspace_root: String,
        /// Absolute path of the file being released.
        #[arg(long)]
        path: String,
        /// Stable agent name (informational; the sentinel is removed
        /// regardless of holder).
        #[arg(long)]
        agent_name: String,
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

fn register_if_needed(
    url: &str,
    name: &str,
    kind: &str,
    parent_session_id: Option<&str>,
) -> Result<HookSession> {
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
    let mut args = serde_json::json!({
        "name": name,
        "kind": kind,
        "pid": pid,
    });
    if let Some(parent) = parent_session_id {
        args["parent_session_id"] = serde_json::Value::String(parent.to_string());
    }
    let result = post_mcp(
        url,
        "tools/call",
        serde_json::json!({
            "name": "register_agent",
            "arguments": args
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

/// `lain hooks claim --url … --path … [--symbol …] [--intent …] [--parent-session-id …]`
pub fn claim(
    url: &str,
    path: &str,
    symbol: &str,
    intent: &str,
    agent_name: &str,
    agent_kind: &str,
    parent_session_id: &str,
) -> Result<()> {
    let parent = if parent_session_id.is_empty() {
        None
    } else {
        Some(parent_session_id)
    };
    let sess = register_if_needed(url, agent_name, agent_kind, parent)?;
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
    let mut args = serde_json::json!({
        "agent_id": sess.agent_id,
        "session_token": sess.session_token,
        "files": files_arr,
    });
    if let Some(parent) = parent {
        args["parent_session_id"] = serde_json::Value::String(parent.to_string());
    }
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

/// `lain hooks release --url … --path … [--parent-session-id …]`
pub fn release(
    url: &str,
    path: &str,
    _symbol: &str,
    agent_name: &str,
    agent_kind: &str,
    parent_session_id: &str,
) -> Result<()> {
    let parent = if parent_session_id.is_empty() {
        None
    } else {
        Some(parent_session_id)
    };
    let sess = register_if_needed(url, agent_name, agent_kind, parent)?;
    let mut args = serde_json::json!({
        "agent_id": sess.agent_id,
        "session_token": sess.session_token,
        "files": [{"path": path}],
    });
    if let Some(parent) = parent {
        args["parent_session_id"] = serde_json::Value::String(parent.to_string());
    }
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

/// Resolve a git ref to its full SHA via `git rev-parse`. The MCP
/// `detect_overlap` tool accepts refs as-is, but resolving here gives
/// the user a clearer error message when the ref is bad *before*
/// paying for the HTTP round-trip, and avoids any ambiguity between
/// "HEAD~1" (rev) and "HEAD" (ref) inside the server's git calls.
fn git_rev_parse_full(ref_str: &str) -> Result<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--verify", ref_str])
        .output()
        .context("failed to run git rev-parse")?;
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "git rev-parse {} failed: {}",
            ref_str,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `lain hooks overlap-check --url … --base … [--head …] --workspace …`
///
/// Resolves `base` (and optional `head`) to full SHAs, then proxies
/// to the server's `detect_overlap` MCP tool. The JSON returned by
/// the server is written verbatim to stdout so shell scripts (e.g.
/// the pre-commit hook) can parse `total_overlaps` and the per-file
/// details directly. Exits non-zero on infrastructure failure so the
/// pre-commit hook can distinguish "no overlap" from "could not run".
pub fn overlap_check(
    url: &str,
    base: &str,
    head: Option<&str>,
    workspace: &str,
) -> Result<()> {
    let base_sha = git_rev_parse_full(base)
        .with_context(|| format!("resolving base ref {base:?}"))?;
    let head_input = head.unwrap_or("HEAD");
    let head_sha = git_rev_parse_full(head_input)
        .with_context(|| format!("resolving head ref {head_input:?}"))?;
    let result = post_mcp(
        url,
        "tools/call",
        serde_json::json!({
            "name": "detect_overlap",
            "arguments": {
                "base": base_sha,
                "head": head_sha,
                "workspace": workspace,
            }
        }),
    )?;
    let parsed = text_of(result)?;
    println!("{}", serde_json::to_string(&parsed)?);
    Ok(())
}

/// `lain hooks lock --workspace-root … --path … --agent-name … …`
///
/// Direct, filesystem-only counterpart to `Claim`. Does NOT contact
/// the lain server: it just writes `<root>/.lain/locks/<sanitized>.json`
/// via `presence_lock::try_lock`. Used by automation that needs the
/// hint layer (operator-readable sentinel, no-daemon coordination)
/// without paying for a full `register_agent` + `claim_files` round
/// trip.
///
/// On success: prints the lock file path to stdout, exits 0.
/// On conflict: prints holder / kind / intent / mtime as JSON to
/// stderr and returns Err so the caller sees a non-zero exit code.
/// `ttl_seconds` is accepted for forward-compatibility but not
/// plumbed through to `try_lock` (which uses a fixed 5s TTL).
pub fn lock(
    workspace_root: &str,
    path: &str,
    agent_name: &str,
    agent_kind: &str,
    intent: &str,
    _ttl_seconds: Option<u64>,
) -> Result<()> {
    let workspace = Path::new(workspace_root);
    let file_path = Path::new(path);
    let agent_id = AgentId(format!("{agent_name}@{}", std::process::id()));
    let kind = AgentKind::parse(agent_kind);
    let parsed_intent = match intent {
        "read" => ClaimIntent::Read,
        _ => ClaimIntent::Edit,
    };
    match presence_lock::try_lock(workspace, file_path, &agent_id, kind, parsed_intent) {
        Ok(lock) => {
            println!("{}", lock.path.display());
            Ok(())
        }
        Err(conflict) => {
            let mtime_unix = conflict
                .mtime()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let body = serde_json::json!({
                "holder": conflict.agent_id().as_str(),
                "kind": conflict.kind().as_str(),
                "intent": match conflict.intent() {
                    ClaimIntent::Read => "read",
                    ClaimIntent::Edit => "edit",
                },
                "mtime_unix": mtime_unix,
                "path": file_path,
            });
            // Print to stderr and return Err so the dispatcher exits
            // non-zero. The pre-commit hook treats any non-zero exit
            // as "infrastructure failure — pass through"; humans
            // running the CLI by hand get a clear conflict message.
            eprintln!("{}", serde_json::to_string(&body)?);
            Err(anyhow::anyhow!(
                "lock for {} already held by {}",
                file_path.display(),
                conflict.agent_id().as_str()
            ))
        }
    }
}

/// `lain hooks unlock --workspace-root … --path … --agent-name …`
///
/// Removes the filesystem sentinel for `path` regardless of holder.
/// Idempotent — ENOENT is treated as success. We compute the sentinel
/// path via `presence_lock::lock_path_for` and remove it via
/// `presence_lock::release_lock_at`, both of which are path-only
/// entry points designed for callers (like this CLI) that don't hold
/// a `FileLock` handle from the matching `lock` invocation.
pub fn unlock(workspace_root: &str, path: &str, _agent_name: &str) -> Result<()> {
    let lock_path = presence_lock::lock_path_for(Path::new(workspace_root), Path::new(path));
    presence_lock::release_lock_at(&lock_path)
        .map_err(|e| anyhow::anyhow!("remove {}: {e}", lock_path.display()))?;
    println!("released {}", lock_path.display());
    Ok(())
}
