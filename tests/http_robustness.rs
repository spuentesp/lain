//! The HTTP transport is the one surface an untrusted local process (or a web
//! page, via a loopback POST) can reach. Boot the real binary and throw
//! hostile requests at it: whatever arrives, the server must answer or close
//! the connection, never crash, and keep serving `/health` afterwards.
//! Cross-origin browser requests must be refused before any tool runs.
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Server {
    child: Child,
    host: String,
    _dirs: Vec<tempfile::TempDir>,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap()
        .success());
}

fn boot() -> Server {
    let project = tempfile::tempdir().unwrap();
    let repo = project.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("a.rs"), "pub fn alpha() -> u32 { 1 }\n").unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "x"]);
    let data = project.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        project.path().join("repos.yaml"),
        format!(
            "data_dir: {}\nrepos:\n  - id: repo\n    source:\n      type: workspace_dir\n      path: {}\n",
            data.display(),
            repo.display()
        ),
    )
    .unwrap();
    std::fs::write(
        project.path().join("workspaces.yaml"),
        "workspaces:\n  - name: w\n    members: [repo]\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let port = free_port();
    let child = Command::new(env!("CARGO_BIN_EXE_lain"))
        .args([
            "server",
            "--transport",
            "http",
            "--port",
            &port.to_string(),
            "--workspace",
            "auto",
            "--config",
        ])
        .arg(project.path().join("repos.yaml"))
        .env("XDG_STATE_HOME", state.path())
        .env("XDG_CONFIG_HOME", cfg.path())
        .env("LAIN_JOB_STORE", state.path().join("jobs.json"))
        .env_remove("LAIN_EMBEDDING_MODEL")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut s = Server {
        child,
        host: format!("127.0.0.1:{port}"),
        _dirs: vec![project, state, cfg],
    };
    let start = Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(60) {
            panic!("server never became healthy");
        }
        if matches!(request(&s.host, health(&s.host).as_bytes()), Some((200, _))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    s.child
        .try_wait()
        .unwrap()
        .is_none()
        .then_some(())
        .expect("server exited during boot");
    s
}

/// Send raw bytes; return (status, body) if any response arrived. `None` means
/// the server closed without answering, which is a legitimate reply to garbage.
fn request(host: &str, raw: &[u8]) -> Option<(u16, String)> {
    let mut s = TcpStream::connect(host).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok();
    s.set_write_timeout(Some(Duration::from_secs(10))).ok();
    let _ = s.write_all(raw);
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text.split_whitespace().nth(1)?.parse().ok()?;
    let body = text
        .find("\r\n\r\n")
        .map(|i| text[i + 4..].to_string())
        .unwrap_or_default();
    Some((status, body))
}

fn health(host: &str) -> String {
    format!("GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
}

fn alive_and_healthy(s: &mut Server) {
    assert!(
        s.child.try_wait().unwrap().is_none(),
        "the server process died"
    );
    let r = request(&s.host, health(&s.host).as_bytes());
    assert!(
        matches!(r, Some((200, _))),
        "/health no longer answers 200: {r:?}"
    );
}

#[test]
fn hostile_requests_never_take_the_server_down() {
    let mut s = boot();
    let big_header = format!(
        "GET /health HTTP/1.1\r\nHost: HOST\r\nX-Pad: {}\r\nConnection: close\r\n\r\n",
        "A".repeat(200_000)
    );
    let mut cases: Vec<Vec<u8>> = vec![
        b"".to_vec(),
        b"\r\n\r\n".to_vec(),
        b"GARBAGE\r\n\r\n".to_vec(),
        b"GET\r\n\r\n".to_vec(),
        b"GET / HTTP/9.9\r\n\r\n".to_vec(),
        b"\x00\x01\x02\xff\xfe\r\n\r\n".to_vec(),
        b"POST /mcp HTTP/1.1\r\nHost: HOST\r\nContent-Length: 99999999999999999999\r\nConnection: close\r\n\r\n".to_vec(),
        b"POST /mcp HTTP/1.1\r\nHost: HOST\r\nContent-Length: 10\r\nConnection: close\r\n\r\nabc".to_vec(), // short body
        b"POST /mcp HTTP/1.1\r\nHost: HOST\r\nContent-Length: 5\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabcde".to_vec(),
        b"POST /mcp HTTP/1.1\r\nHost: HOST\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nZZ\r\nabc\r\n0\r\n\r\n".to_vec(),
        b"POST /mcp HTTP/1.1\r\nHost: HOST\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec(),
        b"GET /../../../../etc/passwd HTTP/1.1\r\nHost: HOST\r\nConnection: close\r\n\r\n".to_vec(),
        b"GET /%00%ff HTTP/1.1\r\nHost: HOST\r\nConnection: close\r\n\r\n".to_vec(),
        b"DELETE /mcp HTTP/1.1\r\nHost: HOST\r\nConnection: close\r\n\r\n".to_vec(),
        b"OPTIONS * HTTP/1.1\r\nHost: HOST\r\nConnection: close\r\n\r\n".to_vec(),
        b"POST /mcp HTTP/1.1\r\nHost: HOST\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_vec(),
        b"POST /mcp HTTP/1.1\r\nHost: HOST\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnull".to_vec(),
        b"POST /mcp HTTP/1.1\r\nHost: HOST\r\nContent-Length: 38\r\nConnection: close\r\n\r\n{\"jsonrpc\":\"2.0\",\"method\":1,\"id\":[[[]]]}".to_vec(),
        big_header.into_bytes(),
    ];
    // Truncations of a valid tool call.
    let valid = b"POST /mcp HTTP/1.1\r\nHost: HOST\r\nContent-Type: application/json\r\nContent-Length: 70\r\nConnection: close\r\n\r\n{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"get_health\"},\"id\":1}";
    for cut in [5, 20, 60, 100, 140, valid.len() - 3] {
        cases.push(valid[..cut.min(valid.len())].to_vec());
    }
    for (i, raw) in cases.iter().enumerate() {
        let raw: Vec<u8> = String::from_utf8_lossy(raw)
            .replace("HOST", &s.host)
            .into_bytes();
        let _ = request(&s.host, &raw);
        assert!(
            s.child.try_wait().unwrap().is_none(),
            "the server died on hostile case {i}: {:?}",
            String::from_utf8_lossy(&raw[..raw.len().min(120)])
        );
    }
    alive_and_healthy(&mut s);
}

#[test]
fn cross_origin_browser_requests_are_refused_before_any_tool_runs() {
    let mut s = boot();
    let body = r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_health","arguments":{}},"id":1}"#;
    for origin in ["http://evil.example", "https://attacker.test:8443", "null"] {
        let raw = format!(
            "POST /mcp HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nContent-Type: text/plain\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
            host = s.host,
            len = body.len()
        );
        let (status, resp) =
            request(&s.host, raw.as_bytes()).expect("a refusal, not a silent close");
        assert!(
            (400..500).contains(&status),
            "origin {origin}: expected a 4xx refusal, got {status}: {resp}"
        );
        assert!(
            !resp.contains("\"tools\"") && !resp.contains("nodes"),
            "origin {origin}: a tool ran: {resp}"
        );
    }
    // DNS rebinding: a Host header that is not loopback.
    let raw = format!(
        "POST /mcp HTTP/1.1\r\nHost: rebind.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    if let Some((status, resp)) = request(&s.host, raw.as_bytes()) {
        assert!(
            (400..500).contains(&status),
            "rebinding Host accepted: {status} {resp}"
        );
    }
    alive_and_healthy(&mut s);
}

#[test]
fn a_same_origin_client_still_works_so_the_guard_is_not_a_blanket_refusal() {
    let s = boot();
    let body = r#"{"jsonrpc":"2.0","method":"ping","id":7}"#;
    let raw = format!(
        "POST /mcp HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        host = s.host,
        len = body.len()
    );
    let (status, resp) = request(&s.host, raw.as_bytes()).expect("response");
    assert_eq!(status, 200, "{resp}");
    let v: serde_json::Value = serde_json::from_str(&resp).expect("json body");
    assert_eq!(v["id"], 7);
    assert_eq!(v["jsonrpc"], "2.0");
}
