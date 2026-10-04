//! Socket session — a `oneshot` JSON-RPC client that talks to a
//! running `lain mcp` over a per-workspace Unix socket (B1,
//! 2026-10-04). Mirrors the `StdioSession` API so the rest of
//! `oneshot.rs` can pick one or the other without
//! restructuring the call loop.

use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{BufRead, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Connect to the socket at `path` and prepare to send/receive
/// JSON-RPC frames. The socket is line-delimited JSON, one
/// frame per line; the reader thread forks so `recv_timeout`
/// can be a synchronous channel.
pub fn connect(path: &std::path::Path) -> Result<SocketSession> {
    let stream = UnixStream::connect(path)
        .with_context(|| format!("connect to socket {}", path.display()))?;
    let read_half = stream
        .try_clone()
        .context("clone unix stream for read half")?;
    let write_half = stream;
    let (tx, rx) = std::sync::mpsc::channel();
    let reader_thread = spawn_reader(read_half, tx);
    Ok(SocketSession {
        write: Mutex::new(write_half),
        rx,
        _reader: Arc::new(reader_thread),
    })
}

pub struct SocketSession {
    write: Mutex<UnixStream>,
    rx: Receiver<Value>,
    /// Keep the reader thread alive for the lifetime of the
    /// session. The thread is detached when the session is
    /// dropped.
    _reader: Arc<thread::JoinHandle<()>>,
}

impl SocketSession {
    pub fn send(&self, value: &Value) -> Result<()> {
        let mut s = serde_json::to_string(value)?;
        s.push('\n');
        let mut guard = self.write.lock().unwrap();
        guard.write_all(s.as_bytes())?;
        guard.flush()?;
        Ok(())
    }

    pub fn recv_timeout(&self, timeout: Duration) -> std::result::Result<Value, RecvTimeoutError> {
        self.rx.recv_timeout(timeout)
    }
}

fn spawn_reader(stream: UnixStream, tx: Sender<Value>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let buf = std::io::BufReader::new(stream);
        for line in buf.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => return,
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };
            if tx.send(value).is_err() {
                return;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write as _;
    use std::os::unix::net::UnixListener;

    #[test]
    fn connect_recv_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("test.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server_thread = thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = String::new();
            let mut reader = std::io::BufReader::new(conn.try_clone().unwrap());
            reader.read_line(&mut buf).unwrap();
            let request: Value = serde_json::from_str(buf.trim()).unwrap();
            let response = json!({
                "jsonrpc": "2.0",
                "id": request.get("id").cloned().unwrap_or(Value::Null),
                "result": "echo",
            });
            let mut s = serde_json::to_string(&response).unwrap();
            s.push('\n');
            conn.write_all(s.as_bytes()).unwrap();
        });

        let session = connect(&socket).unwrap();
        session
            .send(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
            .unwrap();
        let response = session
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert_eq!(response["id"], json!(1));
        assert_eq!(response["result"], "echo");
        server_thread.join().unwrap();
    }
}
