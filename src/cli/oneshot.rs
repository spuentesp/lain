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
//!
//! Two protocol details matter here, both learned from live hangs:
//!
//! 1. stdin must stay OPEN until the `tools/call` response arrives.
//!    Closing it early makes the MCP SDK's stdio reader hit EOF and
//!    tear down the transport; when the single-threaded `lain mcp`
//!    runtime is busy with a startup re-index (cold or stale graph —
//!    i.e. exactly the first-run case), the in-flight `tools/call`
//!    loses the race against the shutdown and never responds.
//! 2. The deadline must be enforced with `recv_timeout` on a reader
//!    thread, not by checking a deadline after a blocking `read()` —
//!    a silent server blocks `read()` forever and the deadline never
//!    fires.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use crate::cli::workspace::find_git_workspace_root;

/// JSON-RPC id of the `tools/call` request (initialize is id 1).
const ID_CALL: i64 = 2;

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
        None => find_git_workspace_root(None)?
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
        let stdin = child.stdin.as_mut().context("take stdin")?;
        // Minimal MCP initialize + tools/call. The MCP spec requires
        // `notifications/initialized` after initialize; we skip it (the
        // server tolerates the omission for short-lived clients).
        // Single source of truth for the protocol version — driven by
        // the `2025_11_25` feature on `rust-mcp-schema`. Bumping the
        // feature in Cargo.toml propagates here without a code change.
        let protocol_version = rust_mcp_schema::ProtocolVersion::latest().to_string();
        let init = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": protocol_version,
                "capabilities": {},
                "clientInfo": {"name": "lain-oneshot", "version": "0.6.0"}
            }
        });
        let call = json!({
            "jsonrpc": "2.0",
            "id": ID_CALL,
            "method": "tools/call",
            "params": {"name": tool, "arguments": args_obj}
        });
        writeln!(stdin, "{}", init)?;
        writeln!(stdin, "{}", call)?;
        // stdin stays OPEN (see module docs): closing it now would let
        // the server's transport shut down before our tools/call is
        // answered whenever a startup re-index keeps the runtime busy.
    }

    // Reader thread: stream stdout lines until the tools/call response
    // shows up, then forward it. Reading line-by-line (not a fixed
    // byte cap) so large responses survive intact.
    let stdout = child.stdout.take().context("take stdout")?;
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break, // EOF or IO error: server gone
                Ok(_) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        if v.get("id").and_then(|i| i.as_i64()) == Some(ID_CALL) {
                            let _ = tx.send(v);
                            break;
                        }
                    }
                }
            }
        }
    });

    // Drain stderr on its own thread so a chatty child (RUST_LOG=debug)
    // can't block on a full pipe buffer; the captured text is attached
    // to error messages for diagnosis.
    let stderr = child.stderr.take().context("take stderr")?;
    let (err_tx, err_rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut s = String::new();
        let _ = stderr.take(256 * 1024).read_to_string(&mut s);
        let _ = err_tx.send(s);
    });

    let deadline = std::time::Duration::from_secs(timeout_secs);
    let tool_response = match rx.recv_timeout(deadline) {
        Ok(v) => v,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            let _ = child.kill();
            let _ = child.wait();
            let stderr_text = err_rx
                .recv_timeout(std::time::Duration::from_millis(200))
                .unwrap_or_default();
            return Err(anyhow!(
                "no tools/call response from `lain mcp` within {timeout_secs}s \
                 (server stderr: {})",
                if stderr_text.trim().is_empty() { "<empty>".into() } else { stderr_text }
            ));
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            let _ = child.kill();
            let _ = child.wait();
            let stderr_text = err_rx
                .recv_timeout(std::time::Duration::from_millis(200))
                .unwrap_or_default();
            return Err(anyhow!(
                "`lain mcp` exited without answering tools/call \
                 (server stderr: {})",
                if stderr_text.trim().is_empty() { "<empty>".into() } else { stderr_text }
            ));
        }
    };

    // Response in hand: now the server is disposable.
    let _ = child.kill();
    let _ = child.wait();

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
