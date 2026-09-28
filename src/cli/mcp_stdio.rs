//! Shared low-level MCP-over-stdio JSON-RPC plumbing.
//!
//! `doctor`, `setup`, and `oneshot` each spawn a `lain` subprocess
//! (the real `mcp` server, or doctor's lightweight `--probe-mcp`),
//! write an `initialize` request, and read JSON-RPC responses off its
//! stdout on a background thread so a deadline can be enforced with
//! `recv_timeout` instead of a blocking `read()` that a silent server
//! would hang forever. Before this module, all three hand-rolled that
//! exact spawn/reader-thread/kill-on-drop scaffolding independently,
//! each with its own timeout and slightly different edge-case
//! handling that had to be kept in sync by hand.
//!
//! This module only provides the plumbing (spawn, send, receive with
//! a deadline, drain stderr, reap the child). Each caller keeps its
//! own policy on top: what timeout to use, what to send after
//! `initialize`, and whether to retry on a `warming_up` gate envelope
//! (only `oneshot` needs that).

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

/// A spawned `lain` subprocess speaking MCP over stdio.
///
/// A background thread parses each stdout line as JSON and forwards
/// any message carrying an `id` field to an internal channel;
/// server-initiated notifications (no `id`) are silently dropped —
/// no current caller needs them. Callers that need to see the
/// `initialize` response themselves (`doctor`, `setup`) just read it
/// as the first message; `oneshot`, which doesn't, skips over it by
/// checking the id itself.
pub struct StdioSession {
    child: Child,
    /// `None` once [`Self::shutdown`] has closed it.
    stdin: Option<ChildStdin>,
    rx: mpsc::Receiver<Value>,
    stderr_rx: Option<mpsc::Receiver<String>>,
}

impl StdioSession {
    /// Spawn `command` with piped stdin/stdout. `capture_stderr`
    /// drains stderr on its own thread (so a chatty child can't block
    /// on a full pipe buffer) and makes its text available via
    /// [`Self::stderr_text`]; otherwise stderr is discarded.
    pub fn spawn(mut command: Command, capture_stderr: bool) -> Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if capture_stderr {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let mut child = command.spawn().context("spawn lain subprocess")?;
        let stdin = child.stdin.take().context("take subprocess stdin")?;
        let stdout = child.stdout.take().context("take subprocess stdout")?;

        let (tx, rx) = mpsc::channel::<Value>();
        std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break, // EOF or IO error: subprocess gone
                    Ok(_) => {
                        if let Ok(v) = serde_json::from_str::<Value>(&line) {
                            if v.get("id").is_some() && tx.send(v).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        });

        let stderr_rx = if capture_stderr {
            let stderr = child.stderr.take().context("take subprocess stderr")?;
            let (err_tx, err_rx) = mpsc::channel::<String>();
            std::thread::spawn(move || {
                use std::io::Read;
                let mut s = String::new();
                let _ = stderr.take(256 * 1024).read_to_string(&mut s);
                let _ = err_tx.send(s);
            });
            Some(err_rx)
        } else {
            None
        };

        Ok(Self {
            child,
            stdin: Some(stdin),
            rx,
            stderr_rx,
        })
    }

    /// Write one JSON-RPC message, newline-terminated, and flush.
    pub fn send(&mut self, value: &Value) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .context("subprocess stdin already closed")?;
        writeln!(stdin, "{value}").context("write to subprocess stdin")?;
        stdin.flush().context("flush subprocess stdin")
    }

    /// Block for the next message carrying an `id` (notifications are
    /// already filtered out by the reader thread), honoring `timeout`.
    pub fn recv_timeout(&self, timeout: Duration) -> std::result::Result<Value, RecvTimeoutError> {
        self.rx.recv_timeout(timeout)
    }

    /// Best-effort captured stderr text; empty if this session wasn't
    /// created with `capture_stderr`, or the drain thread hasn't
    /// finished within a short grace period.
    pub fn stderr_text(&self) -> String {
        self.stderr_rx
            .as_ref()
            .and_then(|rx| rx.recv_timeout(Duration::from_millis(200)).ok())
            .unwrap_or_default()
    }

    /// Stop and reap the subprocess. Idempotent; also runs on `Drop`
    /// as a safety net, but callers should call this explicitly once
    /// they're done so the exit is deterministic rather than tied to
    /// when the `StdioSession` value happens to go out of scope.
    ///
    /// Closes stdin first — an MCP stdio server exits on EOF — and kills
    /// only a child still running after a short grace period. Killing
    /// outright could land while the child was already exiting: under
    /// `cargo llvm-cov` that truncated its profile and failed the merge
    /// ("file header is corrupt").
    pub fn shutdown(&mut self) {
        drop(self.stdin.take());
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for StdioSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The standard `initialize` request every caller sends first.
/// `client_name` identifies the caller in the handshake (e.g.
/// `"lain-doctor"`, `"lain-setup"`, `"lain-oneshot"`).
pub fn initialize_request(id: i64, client_name: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": rust_mcp_schema::ProtocolVersion::latest().to_string(),
            "capabilities": {},
            "clientInfo": {"name": client_name, "version": env!("CARGO_PKG_VERSION")}
        }
    })
}

/// True iff `result` (the `initialize` response's `result` field) has
/// both fields a real MCP server always sets. A stub or misbehaving
/// endpoint that answers `initialize` with something else entirely
/// (rather than erroring outright) is caught here instead of being
/// treated as a successful handshake.
pub fn is_valid_initialize_result(result: &Value) -> bool {
    result.get("serverInfo").is_some() && result.get("protocolVersion").is_some()
}
