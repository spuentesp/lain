//! Per-workspace Unix-socket MCP server (B1, 2026-10-04).
//!
//! `lain mcp --socket PATH` binds a Unix socket in addition to
//! the stdio transport. `lain oneshot` consults this socket
//! first; if the previous process is alive, the new call
//! connects (no reindex), and the warm in-memory graph is
//! shared. The TLA+ spec at `docs/formal/OneshotSharedServer.tla`
//! defines the lifecycle; `OneshotSharedServer-impl.md` is the
//! design doc.
//!
//! ## Protocol
//!
//! The wire format is the same newline-delimited JSON used by
//! the stdio transport: each line is a complete JSON-RPC 2.0
//! frame, terminated by `\n`. The server handles `initialize`,
//! `notifications/initialized`, `tools/list`, and `tools/call`.
//! Anything else returns a JSON-RPC error.
//!
//! ## Lifecycle
//!
//! One accept loop, one connection at a time. The server
//! processes frames sequentially per connection; the
//! `LainServer` is `Arc`-shared with the stdio path so both
//! transports dispatch into the same executor / federation /
//! workspaces / snapshots.

use crate::server::LainServer;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

/// Bind `socket_path`, write the current PID to the sidecar
/// file, and run the accept loop until the process exits or the
/// socket is closed. Returns when the listener is shut down.
pub async fn serve(socket_path: PathBuf, server: Arc<LainServer>) -> Result<()> {
    // Remove a stale socket from a prior crashed process. The
    // `bind` below already errors with EADDRINUSE if a
    // different live process holds it; this is a belt-and-
    // braces for the crash case.
    if socket_path.exists() {
        if let Some(pid) = read_pid_for_socket(&socket_path) {
            if pid_alive(pid) {
                anyhow::bail!(
                    "another `lain mcp` is already running for {} (pid {pid})",
                    socket_path.display()
                );
            }
        }
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_file(pid_path_for(&socket_path));
    }
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("create socket parent dir {}", parent.display())
        })?;
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("bind Unix socket {}", socket_path.display()))?;

    // Write the PID sidecar so `oneshot` can verify liveness
    // before attempting to connect. The current_exe is the
    // lain binary; we report the OS process id.
    let pid = std::process::id();
    std::fs::write(pid_path_for(&socket_path), pid.to_string())
        .with_context(|| format!("write pid file for {}", socket_path.display()))?;

    tracing::info!(
        "shared `lain mcp` socket listening on {} (pid {pid})",
        socket_path.display()
    );

    let result = accept_loop(listener, server).await;

    // Clean up: remove the socket and the pid sidecar on exit.
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_file(pid_path_for(&socket_path));
    result
}

async fn accept_loop(listener: UnixListener, server: Arc<LainServer>) -> Result<()> {
    loop {
        let (stream, _addr) = listener
            .accept()
            .await
            .with_context(|| "accept on shared mcp socket")?;
        let server = server.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, server).await {
                tracing::debug!("socket connection closed: {e}");
            }
        });
    }
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    server: Arc<LainServer>,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            // EOF: client closed.
            return Ok(());
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
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("");
        let params = request
            .get("params")
            .cloned()
            .unwrap_or(Value::Null);

        // A notification has no id and expects no response.
        let is_notification = id.is_none();

        // For a notification, pass `Value::Null` as the id; the
        // dispatch function is uniform.
        let id_value = id.clone().unwrap_or(Value::Null);
        let response = dispatch(method, params, id_value, &server).await;

        if !is_notification {
            write_frame(&mut write_half, &response).await?;
        }
    }
}

async fn dispatch(
    method: &str,
    params: Value,
    id: Value,
    server: &Arc<LainServer>,
) -> Value {
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
        "notifications/initialized" => {
            // Per MCP spec, no response is sent for notifications.
            // Return an empty Value; the caller checks
            // `is_notification` and won't write a frame.
            return Value::Null;
        }
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
                .cloned();
            let result = server
                .ingest()
                .tool_executor()
                .call(&name, arguments.as_ref())
                .await;
            match result {
                Ok(text) => {
                    response["result"] = json!({
                        "content": [{"type": "text", "text": text}],
                        "isError": false,
                    });
                }
                Err(e) => {
                    let err_text = format!("{e}");
                    response["result"] = json!({
                        "content": [{"type": "text", "text": err_text}],
                        "isError": true,
                    });
                }
            }
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

fn pid_path_for(socket_path: &Path) -> PathBuf {
    let mut p = socket_path.to_path_buf();
    let new_name = match p.file_name().and_then(|n| n.to_str()) {
        Some(n) => format!("{n}.pid"),
        None => return p,
    };
    p.set_file_name(new_name);
    p
}

fn read_pid_for_socket(socket_path: &Path) -> Option<u32> {
    std::fs::read_to_string(pid_path_for(socket_path))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_path_appends_dot_pid() {
        let p = pid_path_for(&PathBuf::from("/tmp/lain-mcp-abc.sock"));
        assert_eq!(p, PathBuf::from("/tmp/lain-mcp-abc.sock.pid"));
    }

    #[test]
    fn read_pid_for_socket_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("test.sock");
        std::fs::write(pid_path_for(&socket), "12345").unwrap();
        assert_eq!(read_pid_for_socket(&socket), Some(12345));
    }

    #[test]
    fn read_pid_for_socket_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("missing.sock");
        assert_eq!(read_pid_for_socket(&socket), None);
    }
}
