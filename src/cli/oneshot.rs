//! One-shot MCP query: spawn a transient `lain mcp` server, send a
//! single `tools/call` for the named tool, pretty-print the result,
//! and exit. The ergonomic shortcut for "I just want to grep the
//! symbols without keeping a server alive".
//!
//! Each invocation pays a `lain mcp` startup (~1-5s on first call
//! on a cold cache) so this is for interactive human use, not
//! pipelines. Federation tools that need a `repos.yaml`
//! (`list_repos`, `search_org`, `get_cross_repo_blast_radius`) are
//! NOT available here — those require a federation server. Use the
//! per-repo tools (`find_anchors`, `get_blast_radius`,
//! `find_dead_code`, `get_call_chain`, etc.) which work against the
//! current directory's graph.
//!
//! The server process is killed after a configurable timeout
//! (default 60s, override with `LAIN_ONESHOT_TIMEOUT=<seconds>`)
//! because `lain mcp`'s stdio loop doesn't exit on its own.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Run `lain mcp` as a subprocess, send one `tools/call`, print the
/// result, and exit. Returns an error if the tool name is unknown
/// or the subprocess fails to start.
pub fn run_oneshot(
    workspace: Option<&Path>,
    tool: &str,
    args: &[String],
) -> Result<()> {
    // Walk up for `.git` if --workspace wasn't given — same as
    // `lain mcp` does. The walk lives in `cli::mcp` but we
    // re-implement it inline to keep this module dependency-free.
    let workspace = match workspace {
        Some(p) => p.to_path_buf(),
        None => find_git_workspace()?
            .ok_or_else(|| anyhow!(
                "no `.git` found in any parent directory and no --workspace given; \
                 pass --workspace PATH or run from inside a clone"
            ))?,
    };
    if !workspace.join(".git").exists() {
        return Err(anyhow!(
            "{} has no .git — pass --workspace PATH or run from inside a clone",
            workspace.display()
        ));
    }

    // Build the tool's `arguments` object from the trailing positional
    // args. Heuristic: parse each arg as JSON if possible (so
    // numbers/bools/strings all work), fall back to string. The 90%
    // case is `lain oneshot <tool> <symbol>` so we wrap the first bare
    // arg as `{"symbol": "<arg>"}`. Tools that take more than one
    // arg can be invoked with explicit `key=value` syntax in future
    // iterations; for now the single-symbol shortcut covers
    // `get_blast_radius`, `explain_symbol`, etc.
    let args_obj: Value = if args.is_empty() {
        Value::Object(Default::default())
    } else {
        let first = &args[0];
        let parsed = serde_json::from_str(first).unwrap_or_else(|_| json!(first));
        let mut map = serde_json::Map::new();
        map.insert("symbol".into(), parsed);
        Value::Object(map)
    };

    let timeout_secs: u64 = std::env::var("LAIN_ONESHOT_TIMEOUT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    let exe = std::env::current_exe().context("locate current lain binary")?;

    let mut child = Command::new(exe)
        .arg("mcp")
        .arg("--workspace")
        .arg(&workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("RUST_LOG", "lain=debug")
        .spawn()
        .context("spawn `lain mcp`")?;

    {
        let mut stdin = child.stdin.take().context("take stdin")?;
        // Minimal MCP initialize + tools/call. The MCP spec requires
        // `notifications/initialized` after initialize; we skip it (the
        // server tolerates the omission for short-lived clients).
        let init = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "lain-oneshot", "version": "0.6.0"}
            }
        });
        let id_call: i64 = 2;
        let call = json!({
            "jsonrpc": "2.0",
            "id": id_call,
            "method": "tools/call",
            "params": {"name": tool, "arguments": args_obj}
        });
        writeln!(stdin, "{}", init)?;
        writeln!(stdin, "{}", call)?;
        // Drop stdin so the server's stdio reader hits EOF after the
        // two writes (otherwise `run_stdio` blocks on more input).
    }

    // Read stdout concurrently with a timeout. `wait_with_output`
    // would block until the process exits; we use `try_wait` in a
    // small loop so we can kill the child after the deadline even if
    // it's still alive (which `run_stdio` always is — its stdio loop
    // doesn't exit on EOF).
    let stdout = child.stdout.take().context("take stdout")?;
    let mut reader = std::io::Read::take(stdout, 4096);
    let mut buf = String::new();
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(timeout_secs);
    loop {
        // Best-effort read with a short timeout via poll. If the
        // server is still streaming, we get whatever is buffered; if
        // EOF (unlikely; the server loops), we break.
        use std::io::Read;
        let mut tmp = [0u8; 4096];
        match std::io::Read::read(&mut reader, &mut tmp) {
            Ok(0) => break, // EOF
            Ok(n) => buf.push_str(&String::from_utf8_lossy(&tmp[..n])),
            Err(_) => break,
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        // Brief sleep so we don't spin-busy-read.
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // Kill the server regardless of whether we got a response.
    let _ = child.kill();
    let _ = child.wait();

    // Find the tools/call response (skip the initialize response).
    let id_call: i64 = 2;
    let tool_response = buf
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v.get("id").and_then(|i| i.as_i64()) == Some(id_call))
        .ok_or_else(|| {
            anyhow!(
                "no tools/call response from mcp within {timeout_secs}s; raw output:\n{buf}"
            )
        })?;

    if let Some(err) = tool_response.get("error") {
        return Err(anyhow!("tool error: {err}"));
    }

    let raw_text = tool_response
        .pointer("/result/content/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match serde_json::from_str::<Value>(raw_text) {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap_or_else(|_| raw_text.into())),
        Err(_) => println!("{}", raw_text),
    }
    Ok(())
}

/// Walk up from the current directory until a `.git` directory is
/// found, mirroring `cli::mcp::find_git_workspace_root`. Returns
/// `None` if no `.git` is found within 16 levels.
fn find_git_workspace() -> Result<Option<std::path::PathBuf>> {
    let mut current =
        std::env::current_dir().context("get current dir")?;
    for _ in 0..16 {
        if current.join(".git").exists() {
            return Ok(Some(current));
        }
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => return Ok(None),
        }
    }
    Ok(None)
}
