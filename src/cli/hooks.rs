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
use std::time::Duration;

/// Subcommands for `lain hooks`.
#[derive(Debug, Subcommand)]
pub enum HooksAction {
    /// Claim a file (and optional symbol) for the agent's editing session.
    Claim {
        /// Lain server URL (bare, e.g. `http://localhost:9999`). The MCP
        /// `/mcp` path is appended automatically; a value that already
        /// ends in `/mcp` is accepted unchanged for backwards
        /// compatibility with older hook scripts.
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
        /// Lain server URL (bare, e.g. `http://localhost:9999`). The MCP
        /// `/mcp` path is appended automatically; a value that already
        /// ends in `/mcp` is accepted unchanged for backwards
        /// compatibility with older hook scripts.
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
        /// Lain server URL (bare, e.g. `http://localhost:9999`). The MCP
        /// `/mcp` path is appended automatically; a value that already
        /// ends in `/mcp` is accepted unchanged for backwards
        /// compatibility with older hook scripts.
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
        #[arg(long)]
        agent_kind: String,
        /// Intent — `"edit"` or `"read"`.
        #[arg(long)]
        intent: String,
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

/// Sanitize an agent name so it can be used as a single path segment
/// without escaping the hooks dir. The wishlist call-out was a real
/// traversal: `ORCA_WORKTREE_ID=dc0ac63e-…::/home/sebastian/lain` got
/// passed through and `create_dir_all` cheerfully mirrored `/home/`
/// inside `~/.config/lain/hooks/`. Strategy: keep alphanumerics,
/// `_`, `-`, `.`; collapse everything else to `_`; cap length at 96
/// chars; if the result is empty or starts with `.`, prefix with
/// `_` so it's not a hidden file. Deterministic — same name always
/// maps to the same filename.
fn sanitize_agent_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len().min(96));
    for ch in name.chars() {
        let c = match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' => c,
            _ => '_',
        };
        out.push(c);
        if out.len() >= 96 {
            break;
        }
    }
    if out.is_empty() || out.starts_with('.') {
        out.insert(0, '_');
    }
    out
}

fn session_path(agent_name: &str) -> PathBuf {
    hooks_dir().join(format!("{}.session", sanitize_agent_name(agent_name)))
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

/// Normalize the `--url` flag value to the canonical MCP endpoint URL.
///
/// Accepts both shapes for backwards compatibility with the project-wide
/// `LAIN_URL=http://localhost:9999/mcp` default:
/// - bare server URL (`http://localhost:9999`) → `http://localhost:9999/mcp`
/// - full MCP URL (`http://localhost:9999/mcp`) → unchanged
/// - full MCP URL with trailing slash (`http://localhost:9999/mcp/`) → strip
///
/// The hook scripts and e2e tests now pass the bare form; older callers
/// that still pass the full form continue to work.
fn mcp_endpoint(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    if trimmed.ends_with("/mcp") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/mcp")
    }
}

/// Build the shared reqwest blocking client used by every MCP call.
/// Wired up once with a short total request timeout so a wedged
/// server can't hang the agent hook for the OS's full TCP connect
/// timeout (~75s on Linux). Wishlist #1 (fail open) is meaningless if
/// the hook hangs for a minute before falling through to exit 0.
/// Orca's own hook uses `--connect-timeout 0.5 --max-time 1.5` on
/// curl; we mirror that with a 2-second ceiling.
fn mcp_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .connect_timeout(std::time::Duration::from_millis(500))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new())
}

fn post_mcp(url: &str, method: &'static str, params: serde_json::Value) -> Result<McpResult> {
    let endpoint = mcp_endpoint(url);
    let req = McpRequest {
        jsonrpc: "2.0",
        method,
        params,
        id: 1,
    };
    let client = mcp_client();
    let resp = client.post(&endpoint).json(&req).send().context("HTTP send")?;
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
///
/// Falls back to the filesystem lock layer when no lain server is reachable
/// at `--url`. The wishlist calls this out as the zero-daemon path:
/// subagents and short-lived agents shouldn't have to go through
/// `register_agent` + `heartbeat` just for one edit. When the server is
/// down the filesystem sentinel still serialises edits between agents on
/// the same machine; when the server is up, the in-memory `OccupancyMap`
/// is authoritative and the filesystem write happens as a side-effect.
pub fn claim(
    url: &str,
    path: &str,
    symbol: &str,
    intent: &str,
    agent_name: &str,
    agent_kind: &str,
    parent_session_id: &str,
) -> Result<()> {
    if !server_reachable(url, Duration::from_millis(200)) {
        return claim_filesystem(path, symbol, intent, agent_name, agent_kind);
    }
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
///
/// Same zero-daemon fallback as `claim`: when no server is reachable,
/// remove the filesystem sentinel directly. Idempotent — ENOENT is
/// treated as success.
pub fn release(
    url: &str,
    path: &str,
    _symbol: &str,
    agent_name: &str,
    agent_kind: &str,
    parent_session_id: &str,
) -> Result<()> {
    if !server_reachable(url, Duration::from_millis(200)) {
        return release_filesystem(path);
    }
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

/// Probe whether a lain server is reachable at `--url`. Cheap TCP-level
/// `connect_timeout` against the URL's host:port — no HTTP round-trip,
/// no reqwest runtime to drop inside the surrounding `#[tokio::main]`.
/// Used by `claim`/`release` to decide between the in-memory MCP layer
/// (full coordination) and the filesystem lock fallback (zero daemon).
/// Returns false on any parse, DNS, or connect failure; the caller then
/// falls back to filesystem and the hook stays fail-open.
fn server_reachable(url: &str, timeout: Duration) -> bool {
    let endpoint = mcp_endpoint(url);
    let health_url = format!("{}/health", endpoint.trim_end_matches("/mcp"));
    // Pull host:port out of the URL. We deliberately don't bother with
    // the path / query — only the host:port matters for the TCP probe.
    let scheme_end = health_url.find("://").map(|i| i + 3).unwrap_or(0);
    let authority = &health_url[scheme_end..];
    let authority = authority
        .split('/')
        .next()
        .unwrap_or(authority)
        .split('?')
        .next()
        .unwrap_or(authority);
    let host_port = match authority.rsplit_once(':') {
        Some((h, p)) => {
            // Strip brackets from IPv6 literals; otherwise leave as-is.
            let h = h.trim_start_matches('[').trim_end_matches(']');
            format!("{h}:{p}")
        }
        None => return false,
    };
    let addrs = match std::net::ToSocketAddrs::to_socket_addrs(&host_port) {
        Ok(a) => a,
        Err(_) => return false,
    };
    for addr in addrs {
        if std::net::TcpStream::connect_timeout(&addr, timeout).is_ok() {
            return true;
        }
    }
    false
}

/// Walk up from `path` until we find a `.git` directory; return that
/// ancestor. Falls back to the file's parent directory if no marker
/// is found within 16 levels. `.git` is the only anchor — a previous
/// version honored `.lain/` too, but the zero-daemon fallback
/// creates `<workspace>/.lain/locks/` for any project root and that
/// collides with the "is this a workspace root?" check, leaving
/// stale `.lain` directories in system temp dirs that break later
/// test runs. `.git` is the right anchor: it's never created by
/// lain itself, so the walk-up can't be confused by our own state.
fn find_workspace_root(path: &Path) -> PathBuf {
    let mut current = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| path.to_path_buf());
    for _ in 0..16 {
        if current.join(".git").exists() {
            return current;
        }
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => break,
        }
    }
    // Fallback: file's parent dir. The lock sentinel still works, just
    // scoped to the immediate directory.
    path.parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| path.to_path_buf())
}

/// Filesystem-only counterpart to the in-memory `claim_files` MCP tool.
/// No server round trip, no `register_agent` heartbeat — just an
/// `O_EXCL` write under `<workspace>/.lain/locks/`. The wishlist (#3,
/// #4) wants subagents and one-shot edits to be able to coordinate
/// without a running daemon; this is the path that makes that work.
fn claim_filesystem(
    path: &str,
    _symbol: &str,
    intent: &str,
    agent_name: &str,
    agent_kind: &str,
) -> Result<()> {
    let file_path = Path::new(path);
    let workspace_root = find_workspace_root(file_path);
    let agent_id = AgentId(format!("{agent_name}@{}", std::process::id()));
    let kind = AgentKind::parse(agent_kind);
    let parsed_intent = match intent {
        "read" => ClaimIntent::Read,
        _ => ClaimIntent::Edit,
    };
    match presence_lock::try_lock(&workspace_root, file_path, &agent_id, kind, parsed_intent) {
        Ok(lock) => {
            println!(
                "lain hook: filesystem claim granted at {}",
                lock.path.display()
            );
            Ok(())
        }
        Err(conflict) => {
            let mtime_unix = conflict
                .mtime()
                .duration_since(std::time::UNIX_EPOCH)
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
                "path": path,
            });
            eprintln!("{}", serde_json::to_string(&body)?);
            Err(anyhow::anyhow!(
                "filesystem claim for {} held by {}",
                path,
                conflict.agent_id().as_str()
            ))
        }
    }
}

/// Filesystem-only counterpart to the in-memory `release_files` MCP
/// tool. Idempotent — ENOENT is treated as success. No-op if the
/// sentinel path doesn't resolve to anything on disk (e.g. the file
/// wasn't claimed via the filesystem layer in the first place).
fn release_filesystem(path: &str) -> Result<()> {
    let file_path = Path::new(path);
    let workspace_root = find_workspace_root(file_path);
    let lock_path = presence_lock::lock_path_for(&workspace_root, file_path);
    presence_lock::release_lock_at(&lock_path)
        .map_err(|e| anyhow::anyhow!("remove {}: {e}", lock_path.display()))?;
    println!("released {}", lock_path.display());
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
/// The lock TTL is fixed at 5 seconds by `presence_lock::LOCK_TTL`;
/// stale locks (mtime older than that) can be taken by another agent.
pub fn lock(
    workspace_root: &str,
    path: &str,
    agent_name: &str,
    agent_kind: &str,
    intent: &str,
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

#[cfg(test)]
mod tests {
    use super::{find_workspace_root, mcp_endpoint, sanitize_agent_name, server_reachable};
    use std::time::Duration;

    /// `mcp_endpoint` is the bridge between the `--url` flag (bare server
    /// URL, the new convention) and the post-MCP path that `post_mcp`
    /// needs. Both shapes must round-trip cleanly so existing hook
    /// scripts with `LAIN_URL=http://localhost:9999/mcp` keep working
    /// after the refactor.
    #[test]
    fn mcp_endpoint_handles_both_shapes() {
        // Bare URL → append /mcp.
        assert_eq!(mcp_endpoint("http://localhost:9999"), "http://localhost:9999/mcp");
        // Full URL → unchanged.
        assert_eq!(mcp_endpoint("http://localhost:9999/mcp"), "http://localhost:9999/mcp");
        // Trailing slash on either shape → normalized.
        assert_eq!(mcp_endpoint("http://localhost:9999/"), "http://localhost:9999/mcp");
        assert_eq!(mcp_endpoint("http://localhost:9999/mcp/"), "http://localhost:9999/mcp");
        // Custom host/port + path prefix.
        assert_eq!(
            mcp_endpoint("http lain.local:8080"),
            "http lain.local:8080/mcp"
        );
        assert_eq!(
            mcp_endpoint("https lain.example.com/proxy"),
            "https lain.example.com/proxy/mcp"
        );
    }

    /// `sanitize_agent_name` is the fix for the path-traversal defect
    /// surfaced during the e2e review: `ORCA_WORKTREE_ID=…::/home/...`
    /// used to escape the hooks dir. Path separators and
    /// filesystem-hostile chars must collapse to `_`; the result must
    /// be a single path segment (no `..`, no leading `.`).
    #[test]
    fn sanitize_agent_name_neutralises_traversal_and_weird_chars() {
        assert_eq!(
            sanitize_agent_name("../../../../etc/passwd"),
            "_.._.._.._.._etc_passwd"
        );
        assert_eq!(
            sanitize_agent_name("ORCA_WORKTREE_ID=dc0ac63e-…::/home/sebastian/lain"),
            "ORCA_WORKTREE_ID_dc0ac63e-____home_sebastian_lain"
        );
        assert_eq!(
            sanitize_agent_name("claude-code"),
            "claude-code"
        );
        // Empty and hidden-file names get a leading underscore so they
        // don't disappear or look like dotfiles.
        assert_eq!(sanitize_agent_name(""), "_");
        assert_eq!(sanitize_agent_name("...."), "_....");
        // Caps length at 96 chars to avoid runaway filenames.
        let huge = "a".repeat(500);
        assert_eq!(sanitize_agent_name(&huge).len(), 96);
        // Slashes, backslashes, colons, nulls → all become `_`.
        assert_eq!(
            sanitize_agent_name("a/b\\c:d*e?f\"g<h>i|j\0k"),
            "a_b_c_d_e_f_g_h_i_j_k"
        );
    }

    /// `server_reachable` is the gate between the in-memory MCP layer
    /// and the filesystem fallback. With nothing listening on the URL
    /// it must return false fast (within the timeout). With a live
    /// server it must return true.
    #[test]
    fn server_reachable_is_false_when_no_server() {
        // Pick an unlikely port — 1 is reserved, will not be listening.
        assert!(!server_reachable(
            "http://127.0.0.1:1",
            Duration::from_millis(50),
        ));
    }

    /// `find_workspace_root` is what keeps two agents on the same
    /// machine from writing to different sentinel files for the same
    /// source path. A file under `.git/`'s parent must resolve to that
    /// parent; a file with no `.git` in the tree must fall back to
    /// its immediate parent dir. (`.lain/` is intentionally NOT an
    /// anchor — see `find_workspace_root` doc comment; honoring it
    /// broke a test that was running after a zero-daemon hook run
    /// had littered `.lain/locks/` into a system temp dir.)
    #[test]
    fn find_workspace_root_walks_up_to_git() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Create a fake git repo layout.
        std::fs::create_dir_all(root.join("src/sub")).unwrap();
        std::fs::write(root.join("src/sub/file.rs"), "fn x() {}").unwrap();
        // No .git yet — should fall back to file's parent (src/sub).
        let no_marker = find_workspace_root(&root.join("src/sub/file.rs"));
        assert_eq!(
            no_marker.canonicalize().unwrap(),
            root.join("src/sub").canonicalize().unwrap()
        );
        // Now drop a .git marker one level up — root should win.
        std::fs::create_dir(root.join(".git")).unwrap();
        let with_git = find_workspace_root(&root.join("src/sub/file.rs"));
        assert_eq!(
            with_git.canonicalize().unwrap(),
            root.canonicalize().unwrap()
        );
    }

    /// A `.lain/` directory at any ancestor must NOT be treated as a
    /// workspace anchor — `.lain/locks/` is exactly what the
    /// zero-daemon fallback writes, so a previous run can leave a
    /// `.lain/` in a system temp dir that would otherwise mislead
    /// `find_workspace_root` for the next test. Only `.git` is a
    /// valid anchor.
    #[test]
    fn find_workspace_root_ignores_dot_lain_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src/sub")).unwrap();
        std::fs::write(root.join("src/sub/file.rs"), "fn x() {}").unwrap();
        // A `.lain` directory exists, but no `.git`. `find_workspace_root`
        // must NOT treat `.lain` as a workspace anchor — it falls
        // through to the file's parent (src/sub).
        std::fs::create_dir_all(root.join(".lain/locks")).unwrap();
        let no_marker = find_workspace_root(&root.join("src/sub/file.rs"));
        assert_eq!(
            no_marker.canonicalize().unwrap(),
            root.join("src/sub").canonicalize().unwrap(),
            "`.lain` must not be honored as a workspace anchor"
        );
    }
}
