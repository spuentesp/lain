//! Zero-config shared daemon for `lain mcp` stdio (2026-10-17).
//!
//! Out of the box, N agents launching `{"command":"lain","args":["mcp"]}`
//! share ONE resident daemon: the stdio process either attaches to a
//! live daemon that already serves its workspace set (or a superset) or
//! starts a detached one, then bridges stdin↔socket JSON-RPC to it
//! transparently. A late client whose workspaces are NOT covered hands
//! the running daemon the missing paths via `add_workspaces`, which
//! hot-adds them to the served federation — then bridges. Only one
//! index, one memory, one process group per machine.
//!
//! ## Keying and discovery
//! The daemon's socket is keyed by the resolved workspace SET
//! ([`crate::config::shared_socket_path`]): same set → same socket →
//! start-race resolved by `bind_exclusive` (the loser connects to the
//! winner). Coverage is decided by asking each live daemon its served
//! set over `lain/daemon_info` — a socket filename alone cannot know
//! that a daemon grew past its initial set through `add_workspaces`.
//!
//! ## Fallback contract
//! Every failure path (non-Unix, opted out via `LAIN_MCP_NO_SHARE=1`,
//! socket path too long, daemon failed to come up, `add_workspaces`
//! refused) falls back to the historical private `lain mcp` server, so
//! an agent is never worse off than before.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::server::mcp::socket_server::path_fits;

/// Set by tests (and operators) that want the historical private
/// `lain mcp` server instead of the shared daemon.
pub fn sharing_opt_out() -> bool {
    std::env::var_os("LAIN_MCP_NO_SHARE").is_some()
}

/// How long `add_workspaces` may block the bridge setup while the
/// daemon indexes the handed-over workspaces (the tool itself waits at
/// most 60s per repo and answers `indexing` beyond that).
const ADD_BUDGET: Duration = Duration::from_secs(180);

/// How long a fresh daemon may take to accept its first connection.
/// Matches `oneshot`'s pre-existing startup wait.
fn start_wait_budget() -> Duration {
    Duration::from_secs(60)
}

/// What a live daemon reports on `lain/daemon_info`.
pub struct DaemonInfo {
    pub pid: u32,
    pub workspaces: Vec<PathBuf>,
    pub open_connections: u64,
}

/// One newline-framed JSON-RPC round trip on a short-lived connection.
/// Returns the `result` payload of the reply to `id`. Any timeout or
/// protocol noise fails the whole query — discovery treats that as
/// "no daemon here".
fn rpc_call(stream: &UnixStream, request: &Value, id: i64, budget: Duration) -> Result<Value> {
    let deadline = Instant::now() + budget;
    let mut writer = stream.try_clone().context("clone unix stream")?;
    let mut reader = BufReader::new(stream.try_clone().context("clone unix stream")?);
    let mut line = String::from(serde_json::to_string(request)?);
    line.push('\n');
    writer.write_all(line.as_bytes()).context("write rpc")?;
    writer.flush().context("flush rpc")?;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(anyhow!("rpc timed out waiting for id {id}"));
        }
        stream
            .set_read_timeout(Some(remaining))
            .context("set read timeout")?;
        line.clear();
        let n = reader.read_line(&mut line).context("read rpc reply")?;
        if n == 0 {
            return Err(anyhow!("daemon closed the connection"));
        }
        let value: Value = serde_json::from_str(line.trim())
            .with_context(|| format!("parse rpc reply: {}", line.trim()))?;
        if value.get("id").and_then(|v| v.as_i64()) == Some(id) {
            if let Some(err) = value.get("error") {
                return Err(anyhow!("rpc error: {err}"));
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
        // Notifications and stale ids are skipped, as everywhere else.
    }
}

/// Ask the daemon behind `socket_path` who it is. `None` when nothing
/// answers — dead socket, timeout, or a non-daemon listener.
pub fn query_daemon_info(socket_path: &Path) -> Option<DaemonInfo> {
    let stream = UnixStream::connect(socket_path).ok()?;
    let result = rpc_call(
        &stream,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "lain/daemon_info"}),
        1,
        Duration::from_secs(2),
    )
    .ok()?;
    Some(DaemonInfo {
        pid: result.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32,
        workspaces: result
            .get("workspaces")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(PathBuf::from))
                    .collect()
            })
            .unwrap_or_default(),
        open_connections: result
            .get("open_connections")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

/// Canonicalized, sorted, deduplicated workspace set — the identity
/// both sides compare coverage against.
fn canonical_set(workspaces: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = workspaces
        .iter()
        .map(|w| w.canonicalize().unwrap_or_else(|_| w.clone()))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The result of scanning the runtime dir for resident daemons.
pub enum Discovery {
    /// A live daemon serving a SUPERSET of the client's set can serve
    /// the client as-is.
    Covering { socket: PathBuf },
    /// Some daemon is alive but does not cover the client's set;
    /// `missing` are the client paths it lacks. Rule (6): attach and
    /// hand it those paths instead of spawning a rival daemon.
    Partial {
        socket: PathBuf,
        missing: Vec<PathBuf>,
    },
    /// No live daemon in the runtime dir.
    None,
}

/// Candidate daemon sockets in the run dir: `lain-mcp-<16hex>.sock`,
/// exactly the shape [`crate::config::shared_socket_path`] mints (the
/// reload-signal sockets have a different name shape and are skipped).
fn candidate_sockets() -> Vec<PathBuf> {
    let dir = crate::config::run_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sock"))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix("lain-mcp-"))
                .and_then(|s| s.strip_suffix(".sock"))
                .is_some_and(|stem| {
                    stem.len() == 16 && stem.chars().all(|c| c.is_ascii_alphanumeric())
                })
        })
        .collect();
    out.sort();
    out
}

/// Scan for a daemon that can serve `workspaces` — preferring an
/// existing covering daemon over spawning (rule c), and surfacing the
/// best partial candidate for the `add_workspaces` hand-off path.
pub fn discover(workspaces: &[PathBuf]) -> Discovery {
    let needed = canonical_set(workspaces);
    let mut covering: Vec<(PathBuf, usize)> = Vec::new(); // (socket, served size)
    let mut partial: Option<(PathBuf, usize, Vec<PathBuf>)> = None; // (socket, overlap, missing)

    for socket in candidate_sockets() {
        let Some(info) = query_daemon_info(&socket) else {
            continue;
        };
        let served = canonical_set(&info.workspaces);
        let missing: Vec<PathBuf> = needed
            .iter()
            .filter(|p| !served.contains(p))
            .cloned()
            .collect();
        if missing.is_empty() {
            covering.push((socket, served.len()));
        } else {
            let overlap = needed.len() - missing.len();
            let better = match &partial {
                Some((_, best, _)) => overlap > *best,
                None => true,
            };
            if better {
                partial = Some((socket, overlap, missing));
            }
        }
    }

    // Prefer the exact set-keyed socket when it is among the covering
    // daemons, then the smallest superset, then lexicographic order.
    let exact = crate::config::shared_socket_path(workspaces);
    covering.sort_by_key(|(sock, size)| (*sock != exact, *size, sock.clone()));
    if let Some((socket, _)) = covering.into_iter().next() {
        return Discovery::Covering { socket };
    }
    match partial {
        Some((socket, _, missing)) => Discovery::Partial { socket, missing },
        None => Discovery::None,
    }
}

/// Spawn a detached `lain mcp --daemon` for `workspaces` on
/// `socket_path` (own process group, no stdio — it outlives the
/// caller) and return a connection once its socket accepts. Start
/// race: when the child exits early because another daemon won the
/// bind, connect to the winner (the `oneshot.rs` comment path).
pub fn start_detached_daemon(
    workspaces: &[PathBuf],
    socket_path: &Path,
    wait: Duration,
    embedding_model: Option<&Path>,
    reindex_timeout: Option<std::time::Duration>,
) -> Option<UnixStream> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().ok()?;
    let mut command = Command::new(exe);
    command.arg("mcp");
    for ws in workspaces {
        command.arg("--workspace").arg(ws);
    }
    command.arg("--socket").arg(socket_path).arg("--daemon");
    if let Some(model) = embedding_model {
        command.arg("--embedding-model").arg(model);
    }
    if let Some(timeout) = reindex_timeout {
        command
            .arg("--reindex-timeout")
            .arg(timeout.as_secs().to_string());
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if let Ok(stream) = UnixStream::connect(socket_path) {
            return Some(stream);
        }
        // The daemon died (e.g. another one won the start race and this
        // one exited, or startup failed): stop waiting and try once
        // more — the winner may be the one listening now.
        if let Ok(Some(_)) = child.try_wait() {
            return UnixStream::connect(socket_path).ok();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

/// Hand `paths` to the daemon behind `stream` via `tools/call
/// add_workspaces` and return the tool payload. On `isError` (typed
/// error envelope) the caller falls back.
pub fn admin_add_workspaces(
    stream: &UnixStream,
    paths: &[PathBuf],
    budget: Duration,
) -> Result<Value> {
    let path_strings: Vec<Value> = paths
        .iter()
        .map(|p| json!(p.display().to_string()))
        .collect();
    // `initialize` first, like any MCP client — the daemon's socket
    // dispatch is stateless per frame, but the greeting keeps the
    // conversation spec-shaped.
    rpc_call(
        stream,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                       "clientInfo": {"name": "lain-mcp-bridge", "version": env!("CARGO_PKG_VERSION")}},
        }),
        1,
        budget,
    )?;
    let result = rpc_call(
        stream,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "add_workspaces", "arguments": {"paths": path_strings}},
        }),
        2,
        budget,
    )?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        let text = result
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or("add_workspaces failed");
        return Err(anyhow!("add_workspaces: {text}"));
    }
    let text = result
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or("{}");
    serde_json::from_str(text).with_context(|| format!("parse add_workspaces payload: {text}"))
}

/// Transparent stdin↔socket relay: the stdio process becomes a bridge,
/// preserving every frame verbatim (`initialize`/`tools/list`/
/// `tools/call`/notifications both ways, `serverInfo` included). When
/// stdin hits EOF the socket write half is shut down but replies still
/// in flight are drained; when the daemon goes away the relay stops.
pub async fn run_stdio_bridge(stream: tokio::net::UnixStream) {
    use tokio::io::AsyncWriteExt;
    let (read_half, mut write_half) = stream.into_split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let up = async {
        let _ = tokio::io::copy(&mut stdin, &mut write_half).await;
        let _ = write_half.shutdown().await;
    };
    let down = async {
        let mut read_half = read_half;
        let _ = tokio::io::copy(&mut read_half, &mut stdout).await;
        let _ = stdout.flush().await;
    };
    tokio::pin!(down);
    tokio::select! {
        // Daemon side done: nothing left to forward.
        _ = &mut down => {}
        // stdin done: half-close the socket and drain pending replies.
        _ = up => {
            let _ = down.await;
        }
    }
}

/// Phase A entry point: attach to (or start) the resident shared
/// daemon for `workspaces` and return the connection to bridge on.
/// `None` means "cannot or should not share" — the caller serves
/// privately exactly as before.
pub fn attach_or_start(
    workspaces: &[PathBuf],
    embedding_model: Option<&Path>,
    reindex_timeout: Option<std::time::Duration>,
) -> Option<UnixStream> {
    if sharing_opt_out() {
        return None;
    }
    let socket_key = crate::config::shared_socket_path(workspaces);
    if !path_fits(&socket_key) {
        tracing::warn!(
            "shared daemon socket path {} is too long; serving privately",
            socket_key.display()
        );
        return None;
    }
    match discover(workspaces) {
        Discovery::Covering { socket } => match UnixStream::connect(&socket) {
            Ok(stream) => {
                tracing::info!(
                    "lain mcp: attaching to shared daemon at {}",
                    socket.display()
                );
                Some(stream)
            }
            Err(e) => {
                // Raced a dying daemon: fall through to a fresh start.
                tracing::warn!(
                    "covering daemon at {} vanished ({e}); starting one",
                    socket.display()
                );
                start_detached_daemon(
                    workspaces,
                    &socket_key,
                    start_wait_budget(),
                    embedding_model,
                    reindex_timeout,
                )
            }
        },
        Discovery::Partial { socket, missing } => match UnixStream::connect(&socket) {
            Ok(stream) => {
                tracing::info!(
                    "lain mcp: handing {} workspace(s) to the daemon at {}",
                    missing.len(),
                    socket.display()
                );
                match admin_add_workspaces(&stream, &missing, ADD_BUDGET) {
                    Ok(payload) => {
                        tracing::info!("lain mcp: daemon accepted the config: {payload}");
                        Some(stream)
                    }
                    Err(e) => {
                        tracing::warn!(
                            "add_workspaces on {} failed ({e}); starting a dedicated daemon",
                            socket.display()
                        );
                        drop(stream);
                        start_detached_daemon(
                            workspaces,
                            &socket_key,
                            start_wait_budget(),
                            embedding_model,
                            reindex_timeout,
                        )
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "partial daemon at {} vanished ({e}); starting one",
                    socket.display()
                );
                start_detached_daemon(
                    workspaces,
                    &socket_key,
                    start_wait_budget(),
                    embedding_model,
                    reindex_timeout,
                )
            }
        },
        Discovery::None => {
            tracing::info!(
                "lain mcp: no shared daemon; starting one at {}",
                socket_key.display()
            );
            start_detached_daemon(
                workspaces,
                &socket_key,
                start_wait_budget(),
                embedding_model,
                reindex_timeout,
            )
        }
    }
}
