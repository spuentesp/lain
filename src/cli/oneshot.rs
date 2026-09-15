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

use crate::cli::mcp_stdio::{initialize_request, StdioSession};
use crate::cli::workspace::find_git_workspace_root;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;
use std::sync::mpsc::RecvTimeoutError;

/// JSON-RPC id of the `initialize` request.
const ID_INITIALIZE: i64 = 1;
/// JSON-RPC id of the first `tools/call` request; a retry (see the
/// warming-up handling below) increments from here.
const ID_CALL: i64 = 2;

/// Shut the session down and build an error carrying whatever it
/// printed to stderr, for the three give-up points in the wait loop
/// below (deadline elapsed, recv timed out, subprocess disconnected).
fn give_up(session: &mut StdioSession, message: String) -> anyhow::Error {
    let stderr_text = session.stderr_text();
    session.shutdown();
    anyhow!(
        "{message} (server stderr: {})",
        if stderr_text.trim().is_empty() {
            "<empty>".into()
        } else {
            stderr_text
        }
    )
}

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

    let mut command = Command::new(exe);
    command
        .arg("mcp")
        .arg("--workspace")
        .arg(&workspace)
        .env("RUST_LOG", "lain=debug");
    let mut session = StdioSession::spawn(command, true).context("spawn `lain mcp`")?;

    // Minimal MCP initialize + tools/call. The MCP spec requires
    // `notifications/initialized` after initialize; we skip it (the
    // server tolerates the omission for short-lived clients).
    let init = initialize_request(ID_INITIALIZE, "lain-oneshot");
    let call = json!({
        "jsonrpc": "2.0",
        "id": ID_CALL,
        "method": "tools/call",
        "params": {"name": tool, "arguments": args_obj}
    });
    session.send(&init)?;
    session.send(&call)?;
    // stdin stays OPEN (see module docs): closing it now would let
    // the server's transport shut down before our tools/call is
    // answered whenever a startup re-index keeps the runtime busy.

    // `session`'s reader thread forwards every stdout line that carries
    // a JSON-RPC `id`, including the `initialize` response's — skipped
    // below rather than filtered at the source, since AGENT_UX_ROADMAP.md
    // Milestone 4's central gate can answer a `tools/call` made
    // immediately after `initialize` with a structured `warming_up`
    // result instead of blocking until the startup re-index finishes: a
    // one-shot process has no later chance to poll, so this loop
    // re-sends the same call (with a fresh id) until it gets a real
    // answer.
    let overall_deadline = std::time::Duration::from_secs(timeout_secs);
    let started = std::time::Instant::now();
    let mut next_id = ID_CALL;
    let tool_response = loop {
        let remaining = overall_deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(give_up(
                &mut session,
                format!("no tools/call response from `lain mcp` within {timeout_secs}s"),
            ));
        }
        let response = match session.recv_timeout(remaining) {
            Ok(v) => v,
            Err(RecvTimeoutError::Timeout) => {
                return Err(give_up(
                    &mut session,
                    format!("no tools/call response from `lain mcp` within {timeout_secs}s"),
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(give_up(
                    &mut session,
                    "`lain mcp` exited without answering tools/call".to_string(),
                ))
            }
        };
        if response.get("id").and_then(|i| i.as_i64()) == Some(ID_INITIALIZE) {
            continue; // the initialize response; not interesting here
        }

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
        session.send(&retry_call)?;
    };

    // Response in hand: now the server is disposable.
    session.shutdown();

    if let Some(err) = tool_response.get("error") {
        return Err(anyhow!("tool error: {err}"));
    }

    let raw_text = tool_response
        .pointer("/result/content/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    // MCP signals a tool-level failure (including the central gate's
    // terminal `unavailable_error` — the one gate state this loop lets
    // through instead of retrying, see `is_warming_up` above) via
    // `result.isError`, not the top-level JSON-RPC `error` field checked
    // above. Without this, `lain oneshot` printed the error payload and
    // still exited 0.
    let is_tool_error = tool_response
        .pointer("/result/isError")
        .and_then(|v| v.as_bool())
        == Some(true);

    match serde_json::from_str::<Value>(raw_text) {
        Ok(v) => println!(
            "{}",
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| raw_text.into())
        ),
        Err(_) => println!("{}", raw_text),
    }
    if is_tool_error {
        return Err(anyhow!(
            "tool {tool} returned isError=true (see output above)"
        ));
    }
    Ok(())
}
