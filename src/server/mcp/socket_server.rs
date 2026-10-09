//! Per-workspace Unix-socket MCP server (B1, 2026-10-04). Unix only.
//!
//! A shared `lain mcp` binds a Unix socket; `lain oneshot` connects to it
//! instead of paying a cold start per call. Spec:
//! `docs/formal/OneshotSharedServer.tla`; design: `OneshotSharedServer-impl.md`.
//!
//! ## Protocol
//! Newline-delimited JSON-RPC 2.0, one frame per line, same as stdio. Handled:
//! `initialize`, `notifications/initialized`, `ping`, `tools/list`,
//! `tools/call`. `tools/call` runs the same readiness-gate → dispatch →
//! envelope pipeline as the stdio and HTTP transports
//! ([`crate::server::mcp::handler::call_tool_in_process`]).
//!
//! ## Safety properties (checked by tests below)
//! * **Liveness is a connect probe**, not a PID file. A PID file is wrong
//!   after PID reuse, and `/proc` does not exist on macOS, where the old
//!   check always said "dead" and therefore deleted a live server's socket.
//! * **Access is filesystem permissions**: the socket is `0600` and a
//!   directory this module creates is `0700`. `tools/call` includes tools
//!   that run builds and tests, so anyone who can connect can run them.
//! * **Frames are bounded** ([`MAX_FRAME_BYTES`]): an unterminated line
//!   from a local client cannot grow the server's memory without limit.
//! * **Idle shutdown** (daemon mode): with no connection for the idle
//!   window the server exits and removes its socket, so a one-shot-started
//!   daemon does not live forever.
//! * **Exclusive start**: probe → remove stale socket → bind runs under an
//!   `O_EXCL` guard so two daemons starting together cannot delete each
//!   other's live socket.

use crate::server::LainServer;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

/// Largest accepted request line.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Longest `sun_path` every supported platform accepts (macOS 104, less NUL).
pub const MAX_SOCKET_PATH_BYTES: usize = 103;

/// How long a start guard may exist before it is treated as left by a crash.
const START_GUARD_TTL: Duration = Duration::from_secs(30);

/// True when something is accepting connections on `socket_path`.
pub fn probe_alive(socket_path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket_path).is_ok()
}

/// Whether `socket_path` fits `sun_path` on every supported platform.
pub fn path_fits(socket_path: &Path) -> bool {
    socket_path.as_os_str().len() <= MAX_SOCKET_PATH_BYTES
}

pub(crate) fn pid_path_for(socket_path: &Path) -> PathBuf {
    let mut name = socket_path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".pid");
    socket_path.with_file_name(name)
}

fn guard_path_for(socket_path: &Path) -> PathBuf {
    let mut name = socket_path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".start");
    socket_path.with_file_name(name)
}

/// Removes the start guard when dropped.
struct StartGuard(PathBuf);

impl StartGuard {
    fn take(path: PathBuf) -> Result<Self> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let age = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
                        .unwrap_or_default();
                    if age > START_GUARD_TTL {
                        // Left by a crashed starter; `rename` is atomic, so
                        // exactly one contender removes it.
                        let tomb = path.with_extension(format!("stale-{}", std::process::id()));
                        if std::fs::rename(&path, &tomb).is_ok() {
                            let _ = std::fs::remove_file(&tomb);
                        }
                        continue;
                    }
                    if Instant::now() >= deadline {
                        anyhow::bail!(
                            "another `lain mcp` is starting for this socket; gave up waiting"
                        );
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(e).context("create socket start guard"),
            }
        }
    }
}

impl Drop for StartGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Probe, clear a stale socket, bind, and restrict permissions, all under the
/// start guard. Errors if a live server already owns `socket_path`.
fn bind_exclusive(socket_path: &Path) -> Result<UnixListener> {
    if !path_fits(socket_path) {
        anyhow::bail!(
            "socket path {} is {} bytes; the platform limit is {MAX_SOCKET_PATH_BYTES}. \
             Set XDG_RUNTIME_DIR to a shorter directory.",
            socket_path.display(),
            socket_path.as_os_str().len()
        );
    }
    if let Some(parent) = socket_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .with_context(|| format!("create socket parent dir {}", parent.display()))?;
        }
    }
    let _guard = StartGuard::take(guard_path_for(socket_path))?;
    if socket_path.exists() {
        if probe_alive(socket_path) {
            anyhow::bail!(
                "another `lain mcp` is already serving {}",
                socket_path.display()
            );
        }
        // Nothing is listening: a leftover from a crashed process.
        let _ = std::fs::remove_file(socket_path);
        let _ = std::fs::remove_file(pid_path_for(socket_path));
    }
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("bind Unix socket {}", socket_path.display()))?;
    // `tools/call` can run builds and tests: only the owner may connect.
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restrict permissions on {}", socket_path.display()))?;
    // Diagnostic only; liveness is decided by `probe_alive`.
    let _ = std::fs::write(pid_path_for(socket_path), std::process::id().to_string());
    Ok(listener)
}

/// Bind `socket_path` NOW (so a live server or a bad path is reported to the
/// caller) and serve on a background task until the listener fails or, when
/// `idle` is set, no connection has been open for that long.
pub fn start(
    socket_path: PathBuf,
    server: Arc<LainServer>,
    idle: Option<Duration>,
) -> Result<tokio::task::JoinHandle<Result<()>>> {
    let listener = bind_exclusive(&socket_path)?;
    tracing::info!(
        "shared `lain mcp` socket listening on {} (pid {})",
        socket_path.display(),
        std::process::id()
    );
    Ok(tokio::spawn(async move {
        let result = accept_loop(listener, server, idle).await;
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_file(pid_path_for(&socket_path));
        result
    }))
}

/// [`start`], then wait for the accept loop to finish.
pub async fn serve(
    socket_path: PathBuf,
    server: Arc<LainServer>,
    idle: Option<Duration>,
) -> Result<()> {
    start(socket_path, server, idle)?
        .await
        .context("socket server task failed")?
}

/// Counts open connections and remembers when the last one ended.
struct Activity {
    open: AtomicUsize,
    last: parking_lot::Mutex<Instant>,
}

struct ConnectionGuard(Arc<Activity>);

impl ConnectionGuard {
    fn new(a: &Arc<Activity>) -> Self {
        a.open.fetch_add(1, Ordering::SeqCst);
        *a.last.lock() = Instant::now();
        Self(Arc::clone(a))
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        *self.0.last.lock() = Instant::now();
        self.0.open.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn accept_loop(
    listener: UnixListener,
    server: Arc<LainServer>,
    idle: Option<Duration>,
) -> Result<()> {
    let activity = Arc::new(Activity {
        open: AtomicUsize::new(0),
        last: parking_lot::Mutex::new(Instant::now()),
    });
    loop {
        let accepted = match idle {
            Some(window) => {
                let wait = if activity.open.load(Ordering::SeqCst) == 0 {
                    window.saturating_sub(activity.last.lock().elapsed())
                } else {
                    window
                };
                match tokio::time::timeout(wait, listener.accept()).await {
                    Ok(r) => r,
                    Err(_) => {
                        if activity.open.load(Ordering::SeqCst) == 0
                            && activity.last.lock().elapsed() >= window
                        {
                            tracing::info!("shared `lain mcp` idle for {window:?}; exiting");
                            return Ok(());
                        }
                        continue;
                    }
                }
            }
            None => listener.accept().await,
        };
        let (stream, _addr) = accepted.with_context(|| "accept on shared mcp socket")?;
        let guard = ConnectionGuard::new(&activity);
        let server = server.clone();
        tokio::spawn(async move {
            let _guard = guard;
            if let Err(e) = handle_connection(stream, server).await {
                tracing::debug!("socket connection closed: {e}");
            }
        });
    }
}

/// Read one `\n`-terminated line of at most [`MAX_FRAME_BYTES`]. `Ok(None)` is EOF.
async fn read_frame<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    line: &mut String,
) -> Result<Option<()>> {
    read_frame_limited(reader, line, MAX_FRAME_BYTES).await
}

async fn read_frame_limited<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    line: &mut String,
    max: usize,
) -> Result<Option<()>> {
    line.clear();
    let n = (&mut *reader).take(max as u64 + 1).read_line(line).await?;
    if n == 0 {
        return Ok(None);
    }
    if line.len() > max {
        anyhow::bail!("request frame exceeds {max} bytes");
    }
    Ok(Some(()))
}

async fn handle_connection(stream: tokio::net::UnixStream, server: Arc<LainServer>) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    loop {
        match read_frame(&mut reader, &mut line).await {
            Ok(Some(())) => {}
            Ok(None) => return Ok(()),
            Err(e) => {
                let err = json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32600, "message": format!("invalid request: {e}")},
                    "id": null,
                });
                let _ = write_frame(&mut write_half, &err).await;
                return Err(e);
            }
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let err = json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32700, "message": format!("parse error: {e}")},
                    "id": null,
                });
                write_frame(&mut write_half, &err).await?;
                continue;
            }
        };

        let id = request.get("id").cloned();
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        // A notification has no id and expects no response.
        let is_notification = id.is_none();
        let response = dispatch(method, params, id.unwrap_or(Value::Null), &server).await;
        if !is_notification {
            write_frame(&mut write_half, &response).await?;
        }
    }
}

async fn dispatch(method: &str, params: Value, id: Value, server: &Arc<LainServer>) -> Value {
    let mut response = json!({"jsonrpc": "2.0", "id": id});

    match method {
        "initialize" => {
            response["result"] = json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {
                    "name": "lain",
                    "version": env!("CARGO_PKG_VERSION"),
                    "title": "Lain",
                    "description": "Structural Code Intelligence for AI Agents",
                },
            });
        }
        "notifications/initialized" => return Value::Null,
        "ping" => {
            response["result"] = json!({});
        }
        "tools/list" => {
            let tool_defs = crate::server::mcp::definitions::dump_tools_schema(&[]);
            let tools: Vec<Value> = tool_defs
                .into_iter()
                .map(|def| {
                    json!({
                        "name": def.get("name").cloned().unwrap_or(Value::Null),
                        "description": def.get("description").cloned().unwrap_or(Value::Null),
                        "inputSchema": def.get("inputSchema").cloned().unwrap_or(json!({})),
                    })
                })
                .collect();
            response["result"] = json!({"tools": tools});
        }
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let arguments = params
                .get("arguments")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let result =
                crate::server::mcp::handler::call_tool_in_process(server, &name, arguments).await;
            response["result"] = serde_json::to_value(&result).unwrap_or_else(|e| {
                json!({
                    "content": [{"type": "text", "text": format!("could not serialize result: {e}")}],
                    "isError": true,
                })
            });
        }
        _ => {
            response["error"] = json!({
                "code": -32601,
                "message": format!("method not found: {method}"),
            });
        }
    }
    response
}

async fn write_frame<W: AsyncWriteExt + Unpin>(w: &mut W, frame: &Value) -> Result<()> {
    let mut buf = serde_json::to_string(frame)?;
    buf.push('\n');
    w.write_all(buf.as_bytes()).await?;
    w.flush().await?;
    Ok(())
}

#[cfg(test)]
#[path = "socket_server_verification.rs"]
mod verification;
