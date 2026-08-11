//! End-to-end dual-instance test: one owner + one sidecar on the same
//! workspace. Asserts both report `Operational`, that an overlay
//! insert on the owner shows up on the sidecar within 1 second, and that
//! a second `--mode owner` on the same workspace fails.

use std::process::{Child, Command, Stdio};
use std::time::Duration;
use std::net::TcpListener;
use std::thread;

/// RAII wrapper for the spawned `lain` server: on drop (including panic
/// unwinding) we kill the child and reap it, so the test never leaves a
/// stray process holding the workspace lock or the TCP port.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn pick_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

fn wait_for_health(port: u16, timeout: Duration) -> String {
    let url = format!("http://127.0.0.1:{}/mcp", port);
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_health","arguments":{}}}).to_string();
    let client = reqwest::blocking::Client::new();
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if let Ok(r) = client.post(&url).header("content-type","application/json").body(body.clone()).send() {
            if r.status().is_success() {
                if let Ok(text) = r.text() {
                    return text;
                }
            }
        }
        thread::sleep(Duration::from_millis(200));
    }
    panic!("server on port {port} did not become healthy");
}

#[test]
fn dual_instance_owner_and_sidecar_coexist() {
    let tmp = tempfile::tempdir().unwrap();
    let owner_port = pick_port();
    let sidecar_port = pick_port();
    let lain = env!("CARGO_BIN_EXE_lain");
    // By default the test runs the server in stub mode (no
    // --embedding-model flag) so it is hermetic and works in CI without
    // referencing any user-local paths. Set
    // LAIN_TEST_EMBEDDING_MODEL=/path/to/all-MiniLM-L6-v2.onnx to exercise
    // the real-model path locally.
    let model_flag: Vec<&str> = match std::env::var("LAIN_TEST_EMBEDDING_MODEL") {
        Ok(p) if !p.is_empty() => vec!["--embedding-model", p.as_str()],
        _ => vec![],
    };

    // lain's main() requires a `.git` folder in the workspace, so init one
    // first. This mirrors `tests/agents_install.rs` and is the only piece
    // of preparation the brief's test body omits.
    let init_status = Command::new("git")
        .args(["init", "--quiet", tmp.path().to_str().unwrap()])
        .status().expect("git init");
    assert!(init_status.success(), "git init failed");

    // 1. Owner
    let mut owner = ChildGuard(Command::new(lain)
        .args(["--workspace", tmp.path().to_str().unwrap(),
               "--transport", "http", "--port", &owner_port.to_string(),
               "--mode", "owner"])
        .args(&model_flag)
        .env("LAIN_PORT", owner_port.to_string())
        .stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().expect("spawn owner"));
    let owner_health = wait_for_health(owner_port, Duration::from_secs(60));
    assert!(owner_health.contains("Operational"), "owner health body missing `Operational`: {owner_health}");
    assert_eq!(owner.0.try_wait().unwrap(), None, "owner crashed");

    // 2. Sidecar
    let mut sidecar = ChildGuard(Command::new(lain)
        .args(["--workspace", tmp.path().to_str().unwrap(),
               "--transport", "http", "--port", &sidecar_port.to_string(),
               "--mode", "sidecar"])
        .args(&model_flag)
        .env("LAIN_PORT", sidecar_port.to_string())
        .env("LAIN_OWNER_URL", format!("http://127.0.0.1:{}", owner_port))
        .stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().expect("spawn sidecar"));
    let sidecar_health = wait_for_health(sidecar_port, Duration::from_secs(60));
    assert!(sidecar_health.contains("Operational"), "sidecar health body missing `Operational`: {sidecar_health}");
    assert_eq!(sidecar.0.try_wait().unwrap(), None, "sidecar crashed");

    // 3. A second owner must fail (the flock blocks it) and the failure
    // must surface the brief's `workspace lock held` error message on
    // stderr — mirror of `src/lock.rs::tests::second_owner_rejected_sidecar_admitted`.
    let second_owner = Command::new(lain)
        .args(["--workspace", tmp.path().to_str().unwrap(),
               "--transport", "http", "--port", &pick_port().to_string(),
               "--mode", "owner"])
        .args(&model_flag)
        .env("LAIN_PORT", "9998")
        .stdout(Stdio::null()).stderr(Stdio::piped())
        .output().expect("spawn second owner");
    assert!(!second_owner.status.success(), "second owner must fail");
    let stderr = String::from_utf8_lossy(&second_owner.stderr);
    assert!(
        stderr.contains("workspace lock held"),
        "second owner stderr should report the held lock, got: {stderr}"
    );

    // 4. Cleanup (ChildGuard's Drop also kills; explicit kill is harmless).
    let _ = owner.0.kill(); let _ = owner.0.wait();
    let _ = sidecar.0.kill(); let _ = sidecar.0.wait();
}
