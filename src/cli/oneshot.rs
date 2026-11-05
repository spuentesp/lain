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
//! (default 600s, override with `LAIN_ONESHOT_TIMEOUT=<seconds>`)
//! because `lain mcp`'s stdio loop doesn't exit on its own. The
//! 600s default matches `LAIN_REINDEX_TIMEOUT` so a cold reindex
//! on a non-trivial repo doesn't time out silently.
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
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

/// JSON-RPC id of the `initialize` request.
const ID_INITIALIZE: i64 = 1;
/// JSON-RPC id of the first `tools/call` request; a retry (see the
/// warming-up handling below) increments from here.
const ID_CALL: i64 = 2;

/// The remedy text for a shared daemon this client refuses: the
/// daemon has to be restarted onto the same build, or the caller
/// explicitly asks for a private instance.
#[cfg(unix)]
const ONESHOT_RECOVERY: &str =
    "restart the daemon, or set LAIN_ONESHOT_NO_SHARE=1 to use a private instance";

/// Is the daemon behind `socket` safe for this client to use?
///
/// * `Ok(true)` — it answered and its build identity matches ours.
/// * `Ok(false)` — nothing answered: the caller keeps its previous
///   fallback (no attachment). A daemon that cannot even report its
///   identity is unknown, never silently trusted.
/// * `Err` — a daemon IS there but answers with a different build:
///   the caller must not use it and must not silently start a private
///   indexer beside it.
#[cfg(unix)]
fn shared_daemon_is_usable(socket_path: &Path) -> Result<bool> {
    match crate::cli::shared_daemon::query_daemon_info(socket_path) {
        None => Ok(false),
        Some(info) => match crate::cli::shared_daemon::compatibility_error(&info) {
            Some(_) => Err(crate::cli::shared_daemon::incompatible_daemon_error(
                &info,
                socket_path,
                ONESHOT_RECOVERY,
            )),
            None => Ok(true),
        },
    }
}

/// Connect to a live shared `lain mcp` on `socket_path`, if one is serving.
/// Liveness is whether the socket accepts a connection: a PID file is wrong
/// after PID reuse, and the old `/proc` check made this path Linux-only.
///
/// The daemon must also be the same build as this client: an older
/// daemon would answer with stale code. `Ok(None)` means "no usable
/// shared daemon" (nothing answered) — the caller falls back to
/// starting one or to a private server. `Err` means a daemon with a
/// different build identity is there; the caller must not work around
/// it with a private indexer.
#[cfg(unix)]
fn connect_shared(
    socket_path: &std::path::Path,
) -> Result<Option<crate::cli::socket_session::SocketSession>> {
    if !shared_daemon_is_usable(socket_path)? {
        return Ok(None);
    }
    match crate::cli::socket_session::connect(socket_path) {
        Ok(s) => Ok(Some(s)),
        Err(_) => Ok(None),
    }
}

/// How a detached daemon is obtained: the workspace set it must serve,
/// the socket it must own, and how long to wait for it. Production
/// always uses [`crate::cli::shared_daemon::start_detached_daemon`];
/// the unit test injects a fake that hands back a connection to a
/// daemon that already won the bind race.
#[cfg(unix)]
type DetachedStarter = fn(&[PathBuf], &Path, Duration) -> Option<std::os::unix::net::UnixStream>;

/// [`start_shared_daemon`] with the starter injected, so the start-race
/// branch can be exercised without spawning a daemon.
#[cfg(unix)]
fn start_shared_daemon_with(
    starter: DetachedStarter,
    workspace: &Path,
    socket_path: &Path,
    wait: Duration,
) -> Result<Option<crate::cli::socket_session::SocketSession>> {
    // Generalized over workspace sets in `cli::shared_daemon` (the
    // zero-config multi-agent path spawns the same daemon shape).
    let Some(stream) = starter(
        std::slice::from_ref(&workspace.to_path_buf()),
        socket_path,
        wait,
    ) else {
        return Ok(None);
    };
    if !shared_daemon_is_usable(socket_path)? {
        return Ok(None);
    }
    Ok(crate::cli::socket_session::from_stream(stream).ok())
}

/// Start a detached daemon-mode `lain mcp` for `workspace` and wait until its
/// socket accepts connections. The daemon outlives this process (own process
/// group, no stdio) and exits by itself after an idle window, so the NEXT
/// one-shot connects to the warm graph instead of cold-starting. Returns
/// `Ok(None)` if the daemon exits early or does not come up within `wait`;
/// the caller then falls back to a private stdio server.
///
/// The daemon that won the socket is validated too: a start race can
/// hand us an older daemon that already owned the path, and using it
/// would answer with stale code.
#[cfg(unix)]
fn start_shared_daemon(
    workspace: &Path,
    socket_path: &Path,
    wait: Duration,
) -> Result<Option<crate::cli::socket_session::SocketSession>> {
    start_shared_daemon_with(
        |workspaces, socket_path, wait| {
            crate::cli::shared_daemon::start_detached_daemon(
                workspaces,
                socket_path,
                wait,
                None,
                None,
                false,
            )
        },
        workspace,
        socket_path,
        wait,
    )
}

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

/// Run the call loop using an already-connected `SocketSession`
/// against a shared `lain mcp`. Mirrors the stdio call loop
/// but with the simpler `&self` session API. Returns `Ok(())`
/// after printing the tool's result.
#[cfg(unix)]
fn run_call_loop_via_socket(
    session: crate::cli::socket_session::SocketSession,
    tool: &str,
    args_obj: Value,
    timeout_secs: u64,
) -> Result<()> {
    let init = initialize_request(ID_INITIALIZE, "lain-oneshot");
    let call_id = ID_CALL;
    let call = json!({
        "jsonrpc": "2.0",
        "id": call_id,
        "method": "tools/call",
        "params": {"name": tool, "arguments": args_obj}
    });
    session.send(&init)?;
    session.send(&call)?;

    let overall_deadline = Duration::from_secs(timeout_secs);
    let started = std::time::Instant::now();
    let mut next_id = call_id;
    let tool_response = loop {
        let remaining = overall_deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(anyhow!(
                "no tools/call response from shared `lain mcp` within {timeout_secs}s"
            ));
        }
        let response = match session.recv_timeout(remaining) {
            Ok(v) => v,
            Err(RecvTimeoutError::Timeout) => {
                return Err(anyhow!(
                    "no tools/call response from shared `lain mcp` within {timeout_secs}s"
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(anyhow!(
                    "shared `lain mcp` exited without answering tools/call"
                ));
            }
        };
        if response.get("id").and_then(|i| i.as_i64()) == Some(ID_INITIALIZE) {
            continue;
        }
        let gate_envelope = response
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
            .and_then(|text| serde_json::from_str::<Value>(text).ok());
        let partial = response
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
            .is_some_and(|t| t.contains(crate::server::mcp::handler::INDEXING_NOTE_MARKER));
        let is_warming_up = partial
            || gate_envelope
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
            continue;
        }
        std::thread::sleep(Duration::from_millis(retry_after_ms).min(remaining_after_retry));
        next_id += 1;
        let retry_call = json!({
            "jsonrpc": "2.0",
            "id": next_id,
            "method": "tools/call",
            "params": {"name": tool, "arguments": args_obj}
        });
        session.send(&retry_call)?;
    };

    if let Some(err) = tool_response.get("error") {
        return Err(anyhow!("tool error: {err}"));
    }
    let raw_text = tool_response
        .pointer("/result/content/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or("");
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
            let base = "no `.git` found in any parent directory and no --workspace given; \
                 pass --workspace PATH or run from inside a clone";
            match crate::cli::workspace::federation_root_guidance_here() {
                Some(guidance) => anyhow!("{base}; {guidance}"),
                None => anyhow!("{base}"),
            }
        })?,
    };
    if !workspace.join(".git").exists() {
        let base = format!(
            "{} has no .git — pass --workspace PATH or run from inside a clone",
            workspace.display()
        );
        return Err(
            match crate::cli::workspace::federation_root_guidance(&workspace) {
                Some(guidance) => anyhow!("{base}; {guidance}"),
                None => anyhow!("{base}"),
            },
        );
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
        let schema = tool_input_schema(tool);
        let mut map = serde_json::Map::new();
        for arg in args {
            if let Some((k, v)) = arg.split_once('=') {
                map.insert(k.to_string(), typed_value(schema.as_ref(), k, v)?);
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
        // Form 3: bare positionals, assigned to the tool's required
        // arguments in the order its schema declares them:
        // `get_call_chain login helper` -> {from, to}, `find_symbol x` ->
        // {name}. Only the first was used, always as `symbol`, so both of
        // those failed with "missing required argument".
        positional_args(tool, args)?
    };

    let timeout_secs: u64 = std::env::var("LAIN_ONESHOT_TIMEOUT")
        .ok()
        .and_then(|s| s.parse().ok())
        // 60s is too short for a cold reindex on a non-trivial repo
        // (Lain-on-Lain in 2026-10-04: ~5 min for 41k LOC + LSP
        // prewarm). The error "no tools/call response from `lain
        // mcp` within 60s" doesn't tell the user whether the server
        // is busy indexing or hung, so they have no signal that 60s
        // is a too-small budget. 600s is the same default the
        // reindex path already uses (`LAIN_REINDEX_TIMEOUT`).
        // Override with `LAIN_ONESHOT_TIMEOUT=<seconds>` for tighter
        // pipelines.
        .unwrap_or(600);

    // B1 (2026-10-04): share one warm `lain mcp` across one-shot calls. Use a
    // live shared server if there is one; otherwise start a detached daemon
    // (it outlives this process and exits when idle) and talk to that. Any
    // failure, a platform without Unix sockets, a socket path too long for
    // the platform, or `LAIN_ONESHOT_NO_SHARE=1` falls back to the private
    // stdio server below.
    //
    // A shared daemon whose build identity differs from this client's is
    // NOT one of those fallbacks: it would answer with stale code, and
    // quietly serving the call from a private indexer would double the
    // machine's indexing load. The call fails with the socket and both
    // identities instead; `LAIN_ONESHOT_NO_SHARE=1` is the explicit way
    // to ask for a private instance.
    #[cfg(unix)]
    {
        let socket_path = crate::config::oneshot_socket_path(&workspace);
        let sharing = std::env::var_os("LAIN_ONESHOT_NO_SHARE").is_none()
            && crate::server::mcp::socket_server::path_fits(&socket_path);
        if sharing {
            let session = match connect_shared(&socket_path)? {
                Some(session) => Some(session),
                None => start_shared_daemon(
                    &workspace,
                    &socket_path,
                    Duration::from_secs(timeout_secs.min(60)),
                )?,
            };
            if let Some(session) = session {
                return run_call_loop_via_socket(session, tool, args_obj, timeout_secs);
            }
            tracing::warn!("shared `lain mcp` unavailable; using a private server");
        }
    }

    // Private server for this call only (stdio; it dies with this process).
    let exe = std::env::current_exe().context("locate current lain binary")?;

    let mut command = Command::new(exe);
    command
        .arg("mcp")
        .arg("--workspace")
        .arg(&workspace)
        // This IS the private path (opted out of sharing above, or a
        // platform without sockets): the spawned `lain mcp` must not
        // bridge out to a shared daemon either.
        .env("LAIN_MCP_NO_SHARE", "1")
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
        // An ungated tool answers mid-index with a partial result and a
        // "still indexing" note. A persistent client can ask again later;
        // a one-shot cannot, so wait for the whole index like the gate does.
        let partial = response
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
            .is_some_and(|t| t.contains(crate::server::mcp::handler::INDEXING_NOTE_MARKER));
        let is_warming_up = partial
            || gate_envelope
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

/// Map bare positional arguments onto `tool`'s required arguments,
/// then (when it has none) onto its optional string arguments in
/// declaration order — e.g. `lain oneshot find_dead_code <like>`.
fn positional_args(tool: &str, args: &[String]) -> Result<Value> {
    let schema_owned = tool_input_schema(tool);
    let schema = schema_owned.as_ref();
    let required: Vec<String> = schema
        .and_then(|s| s.get("required"))
        .and_then(|r| r.as_array())
        .map(|r| {
            r.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let has_property = |name: &str| {
        schema
            .and_then(|s| s.get("properties"))
            .and_then(|p| p.get(name))
            .is_some()
    };
    let names: Vec<String> = if !required.is_empty() {
        required
    } else if has_property("symbol") || schema.is_none() {
        vec!["symbol".to_string()]
    } else {
        schema
            .and_then(|s| s.get("properties"))
            .and_then(|p| p.as_object())
            .map(|p| {
                p.keys()
                    .filter(|k| {
                        p.get(*k)
                            .and_then(|v| v.get("type"))
                            .and_then(|t| t.as_str())
                            == Some("string")
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    };
    if args.len() > names.len() {
        anyhow::bail!(
            "`{tool}` takes {} positional argument(s) ({}); got {}. Use key=value for the others.",
            names.len(),
            if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            },
            args.len()
        );
    }
    let mut map = serde_json::Map::new();
    for (name, raw) in names.iter().zip(args) {
        map.insert(name.clone(), typed_value(schema, name, raw)?);
    }
    Ok(Value::Object(map))
}

fn tool_input_schema(tool: &str) -> Option<Value> {
    crate::server::mcp::definitions::dump_tools_schema(&[])
        .into_iter()
        .find(|t| t.get("name").and_then(|n| n.as_str()) == Some(tool))
        .and_then(|t| t.get("inputSchema").cloned())
}

/// A command-line value converted to the type the tool's schema declares
/// for `name`. Guessing from the text turned `find_symbol 2024` into a
/// number the tool refused, and let `limit=abc` through as a string the
/// tool silently ignored.
fn typed_value(schema: Option<&Value>, name: &str, raw: &str) -> Result<Value> {
    let declared = schema
        .and_then(|s| s.get("properties"))
        .and_then(|p| p.get(name))
        .and_then(|p| p.get("type"))
        .and_then(|t| t.as_str());
    match declared {
        Some("string") => Ok(json!(raw)),
        Some("integer") => raw
            .parse::<i64>()
            .map(|n| json!(n))
            .map_err(|_| anyhow!("`{name}` must be an integer, got '{raw}'")),
        Some("number") => raw
            .parse::<f64>()
            .map(|n| json!(n))
            .map_err(|_| anyhow!("`{name}` must be a number, got '{raw}'")),
        Some("boolean") => raw
            .parse::<bool>()
            .map(|b| json!(b))
            .map_err(|_| anyhow!("`{name}` must be true or false, got '{raw}'")),
        // Arrays, objects, or no schema: JSON if it parses, else text.
        _ => Ok(serde_json::from_str(raw).unwrap_or_else(|_| json!(raw))),
    }
}

#[cfg(test)]
mod positional_tests {
    use super::*;

    #[test]
    fn optional_string_args_take_positionals_when_nothing_is_required() {
        let v = positional_args("find_dead_code", &["auth handler".into()]).unwrap();
        assert_eq!(v, json!({"like": "auth handler"}));
    }

    #[test]
    fn values_follow_the_schema_types() {
        let v = positional_args("find_symbol", &["2024".into()]).unwrap();
        assert_eq!(v, json!({"name": "2024"}));
        let schema = tool_input_schema("find_anchors");
        assert!(typed_value(schema.as_ref(), "limit", "abc").is_err());
        assert_eq!(
            typed_value(schema.as_ref(), "limit", "5").unwrap(),
            json!(5)
        );
    }

    #[test]
    fn positionals_follow_the_required_arguments() {
        let v = positional_args("get_call_chain", &["login".into(), "helper".into()]).unwrap();
        assert_eq!(v, json!({"from": "login", "to": "helper"}));
        let v = positional_args("find_symbol", &["helper".into()]).unwrap();
        assert_eq!(v, json!({"name": "helper"}));
        let v = positional_args("get_blast_radius", &["helper".into()]).unwrap();
        assert_eq!(v, json!({"symbol": "helper"}));
        assert!(positional_args("find_symbol", &["a".into(), "b".into()]).is_err());
    }
}

/// The start-race branch: the detached start can hand back a
/// connection to a daemon that ALREADY owns the socket — an older
/// build. It must be refused exactly like the existing-socket path.
#[cfg(all(test, unix))]
mod start_race_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    /// A fake daemon that owns `socket`, answers `lain/daemon_info`
    /// with a STALE build identity, and records every method it was
    /// asked for. One thread per connection: the identity probe and
    /// the (silent) start-race connection arrive independently.
    struct StaleDaemon {
        stop: Arc<AtomicBool>,
        methods: Arc<Mutex<Vec<String>>>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl StaleDaemon {
        fn start(socket: &Path) -> Self {
            let listener = UnixListener::bind(socket).unwrap();
            listener.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let methods = Arc::new(Mutex::new(Vec::new()));
            let handle = {
                let stop = Arc::clone(&stop);
                let methods = Arc::clone(&methods);
                std::thread::spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                let methods = Arc::clone(&methods);
                                std::thread::spawn(move || {
                                    let mut writer = stream.try_clone().unwrap();
                                    let mut reader = BufReader::new(stream);
                                    loop {
                                        let mut line = String::new();
                                        match reader.read_line(&mut line) {
                                            Ok(0) | Err(_) => break,
                                            Ok(_) => {}
                                        }
                                        let Ok(request) =
                                            serde_json::from_str::<Value>(line.trim())
                                        else {
                                            continue;
                                        };
                                        let method = request
                                            .get("method")
                                            .and_then(|m| m.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        methods.lock().unwrap().push(method.clone());
                                        let id = request.get("id").cloned().unwrap_or(Value::Null);
                                        let result = match method.as_str() {
                                            "lain/daemon_info" => json!({
                                                "pid": 999,
                                                "version": crate::server::build_info::VERSION,
                                                "build_commit": "stale",
                                                "index_build_id": "stale",
                                                "workspaces": [],
                                                "open_connections": 0,
                                                "shared": true,
                                            }),
                                            _ => json!({}),
                                        };
                                        let response = json!({
                                            "jsonrpc": "2.0", "id": id, "result": result
                                        });
                                        if writeln!(writer, "{response}").is_err() {
                                            break;
                                        }
                                        let _ = writer.flush();
                                    }
                                });
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(10));
                            }
                            Err(e) => panic!("fake daemon accept failed: {e}"),
                        }
                    }
                })
            };
            StaleDaemon {
                stop,
                methods,
                handle: Some(handle),
            }
        }

        /// Every method the daemon was asked for, in order.
        fn methods(&self) -> Vec<String> {
            self.methods.lock().unwrap().clone()
        }
    }

    impl Drop for StaleDaemon {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    /// A stale daemon that won the bind race must be refused:
    ///
    /// * **identity-only communication** — the daemon is probed for its
    ///   identity and never called;
    /// * **refusal with expected/actual values** — the error names
    ///   both identities and the socket;
    /// * **no private fallback** — the result is an ERROR, not
    ///   `Ok(None)` (which is what would make the caller serve the
    ///   call from a private indexer).
    ///
    /// The starter is injected, so no daemon is spawned and the test
    /// does not depend on timing.
    #[test]
    fn a_stale_start_race_winner_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("lain-mcp-race.sock");
        let daemon = StaleDaemon::start(&socket);

        // The socket is already owned, so the "detached start" hands
        // back a connection to the stale winner instead of spawning
        // a daemon of its own.
        let starter: DetachedStarter =
            |_workspaces, socket_path, _wait| UnixStream::connect(socket_path).ok();

        let error =
            match start_shared_daemon_with(starter, dir.path(), &socket, Duration::from_secs(1)) {
                Ok(_) => panic!("a stale race winner must be refused, not used"),
                Err(e) => e,
            };

        let message = error.to_string();
        assert!(
            message.contains("incompatible shared daemon"),
            "the refusal must name the incompatibility: {message}"
        );
        assert!(
            message.contains("stale"),
            "the refusal must name the ACTUAL identity: {message}"
        );
        assert!(
            message.contains(crate::server::build_info::VERSION),
            "the refusal must name the EXPECTED identity: {message}"
        );
        assert!(
            message.contains(&socket.display().to_string()),
            "the refusal must name the socket: {message}"
        );
        assert_eq!(
            daemon.methods(),
            vec!["lain/daemon_info".to_string()],
            "the stale daemon must only be probed for its identity, never called"
        );
    }
}
