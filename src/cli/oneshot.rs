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

use crate::cli::workspace::find_git_workspace_root;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// JSON-RPC id of the `initialize` request.
const ID_INITIALIZE: i64 = 1;
/// JSON-RPC id of the first `tools/call` request; a retry (see the
/// warming-up handling below) increments from here.
const ID_CALL: i64 = 2;

/// Run `lain mcp` as a subprocess, send one `tools/call`, print the
/// result, and exit. Returns an error if the tool name is unknown
/// or the subprocess fails to start.
pub fn run_oneshot(workspace: Option<&Path>, tool: &str, args: &[String]) -> Result<()> {
    // Walk up for `.git` if --workspace wasn't given — same as
    // `lain mcp` does. The walk lives in `cli::mcp` but we
    // re-implement it inline to keep this module dependency-free.
    let workspace = match workspace {
        Some(p) => p.to_path_buf(),
        None => find_git_workspace_root(None)?.ok_or_else(|| {
            anyhow!(
                "no `.git` found in any parent directory and no --workspace given; \
                 pass --workspace PATH or run from inside a clone"
            )
        })?,
    };
    if !workspace.join(".git").exists() {
        return Err(anyhow!(
            "{} has no .git — pass --workspace PATH or run from inside a clone",
            workspace.display()
        ));
    }

    // Build the tool's `arguments` object from the trailing positional
    // args. Three forms are supported, in priority order:
    //
    //  1. `key=value` pairs: `lain oneshot get_call_chain from=main to=foo`
    //     inserts `{"from": "main", "to": "foo"}` verbatim.
    //  2. JSON objects: `lain oneshot '{"symbol":"foo","limit":2}'`
    //     parses as a single object. Lets a caller pass nested args.
    //  3. Bare positional: `lain oneshot <tool> <symbol>` wraps the
    //     first bare arg as `{"symbol": "<arg>"}` — the 90% case.
    //
    // Pre-fix, only form 3 worked (and only `args[0]` was kept, with
    // `args[1..]` silently dropped). Tools requiring `from`+`to`,
    // `path`+`limit`, or any other multi-arg combination failed
    // immediately. The new form-1 path covers the common
    // human-invoked case; form 2 covers the JSON-scripted case.
    let args_obj: Value = if args.is_empty() {
        Value::Object(Default::default())
    } else if !args.is_empty() && args.iter().all(|a| a.contains('=')) {
        // Form 1: every arg is `key=value`. Each pair is split on the
        // first `=` so values can contain `=` (rare but possible
        // for base64 or paths). Values are parsed as JSON where
        // possible (so `limit=5` lands as a number, `active=true`
        // as a bool); otherwise left as strings. This mirrors the
        // behavior callers get from a JSON object.
        let mut map = serde_json::Map::new();
        for arg in args {
            if let Some((k, v)) = arg.split_once('=') {
                let parsed = serde_json::from_str(v).unwrap_or_else(|_| json!(v));
                map.insert(k.to_string(), parsed);
            }
        }
        Value::Object(map)
    } else if args.len() == 1 && (args[0].trim_start().starts_with('{') || args[0].starts_with('['))
    {
        // Form 2: a single JSON object/array. Pass through verbatim
        // (after a parse check so we surface a clear error rather
        // than a tool-side failure deep inside the MCP server).
        serde_json::from_str(&args[0])
            .with_context(|| format!("parse JSON object from oneshot arg: {}", args[0]))?
    } else {
        // Form 3: bare positional. Wrap the first bare arg as
        // `{"symbol": <arg>}` for the common single-symbol tools.
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
            "id": ID_INITIALIZE,
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

    // Reader thread: forward every stdout line that carries a JSON-RPC
    // `id` other than the `initialize` request's, for as long as the
    // child keeps writing. AGENT_UX_ROADMAP.md Milestone 4's central
    // gate can answer a `tools/call` made immediately after `initialize`
    // with a structured `warming_up` result instead of blocking until
    // the startup re-index finishes — a one-shot process has no later
    // chance to poll, so the loop below re-sends the same call until it
    // gets a real answer, and needs to keep watching stdout across every
    // retry rather than stopping after the first `id` it sees. A
    // notification (`notifications/lain/capabilities_changed`, no `id`
    // field) is silently skipped here; a one-shot invocation has no use
    // for it.
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
                        let id = v.get("id").and_then(|i| i.as_i64());
                        if let Some(id) = id {
                            if id != ID_INITIALIZE && tx.send(v).is_err() {
                                break;
                            }
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

    let overall_deadline = std::time::Duration::from_secs(timeout_secs);
    let started = std::time::Instant::now();
    let mut next_id = ID_CALL;
    let tool_response = loop {
        let remaining = overall_deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            let _ = child.kill();
            let _ = child.wait();
            let stderr_text = err_rx
                .recv_timeout(std::time::Duration::from_millis(200))
                .unwrap_or_default();
            return Err(anyhow!(
                "no tools/call response from `lain mcp` within {timeout_secs}s \
                 (server stderr: {})",
                if stderr_text.trim().is_empty() {
                    "<empty>".into()
                } else {
                    stderr_text
                }
            ));
        }
        let response = match rx.recv_timeout(remaining) {
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
                    if stderr_text.trim().is_empty() {
                        "<empty>".into()
                    } else {
                        stderr_text
                    }
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
                    if stderr_text.trim().is_empty() {
                        "<empty>".into()
                    } else {
                        stderr_text
                    }
                ));
            }
        };

        // A `graph_required`/`semantic_required` tool called before the
        // index is `ready` comes back as the central gate's structured
        // `warming_up` envelope, not the real answer — retry the exact
        // same call instead of printing a placeholder and exiting. This
        // is the one-shot equivalent of the poll loop a persistent MCP
        // client is expected to run against `get_capabilities`.
        let gate_envelope = response
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
            .and_then(|text| serde_json::from_str::<Value>(text).ok());
        let is_warming_up = gate_envelope
            .as_ref()
            .and_then(|v| v.get("state"))
            .and_then(|s| s.as_str())
            == Some("warming_up");
        if !is_warming_up {
            break response;
        }
        let retry_after_ms = gate_envelope
            .as_ref()
            .and_then(|v| v.get("retry_after_ms"))
            .and_then(|r| r.as_u64())
            .unwrap_or(1000);
        let remaining_after_retry = overall_deadline.saturating_sub(started.elapsed());
        if remaining_after_retry.is_zero() {
            continue; // let the top of the loop raise the timeout error
        }
        std::thread::sleep(
            std::time::Duration::from_millis(retry_after_ms).min(remaining_after_retry),
        );
        next_id += 1;
        let retry_call = json!({
            "jsonrpc": "2.0",
            "id": next_id,
            "method": "tools/call",
            "params": {"name": tool, "arguments": args_obj}
        });
        let stdin = child.stdin.as_mut().context("re-take stdin for retry")?;
        writeln!(stdin, "{}", retry_call)?;
        stdin.flush()?;
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
        Ok(v) => println!(
            "{}",
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| raw_text.into())
        ),
        Err(_) => println!("{}", raw_text),
    }
    Ok(())
}
