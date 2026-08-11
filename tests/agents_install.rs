//! End-to-end harness: install every supported agent into a temp HOME,
//! then run `lain agents verify --all` against a temp Lain instance.

use std::process::{Child, Command, Stdio};

fn lain_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_lain"))
}

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

#[test]
fn install_and_verify_for_supported_agents() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let port = pick_port();
    // Bind the XDG path to a local so its `to_str()` borrow is valid for the
    // entire `env_overrides` lifetime — `vec![]` does not extend temporaries.
    let xdg_config_home = tmp.path().join(".config");
    let env_overrides: Vec<(&str, &str)> = vec![
        ("HOME", tmp.path().to_str().unwrap()),
        ("XDG_CONFIG_HOME", xdg_config_home.to_str().unwrap()),
        ("LAIN_PORT", Box::leak(port.to_string().into_boxed_str()) as &str),
    ];

    // 1. Start a fresh Lain server on the chosen port.
    // Lain's main() bails when the workspace lacks `.git`, so init one first.
    let init_status = Command::new("git")
        .args(["init", "--quiet", tmp.path().to_str().unwrap()])
        .status().expect("git init");
    assert!(init_status.success(), "git init failed");
    // The test hardcodes a user-local model path. In CI the file does
    // not exist, so pass --embedding-model only when present; otherwise
    // the server falls back to stub mode and starts fast (the test only
    // needs get_health to respond, which works in stub mode).
    let model = "/home/sebastian/.local/lain/models/all-MiniLM-L6-v2.onnx";
    let model_flag: &[&str] = if std::path::Path::new(model).exists() {
        &["--embedding-model", model]
    } else {
        &[]
    };
    let mut server = ChildGuard(Command::new(lain_bin())
        .args(["--workspace", tmp.path().to_str().unwrap(),
               "--transport", "http", "--port", &port.to_string()])
        .args(model_flag)
        .envs(env_overrides.iter().copied())
        .stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().expect("spawn lain"));
    wait_for_health(&port);

    // 2. Install Claude and Kimi (smoke test of the two most-used agents).
    for id in ["claude", "kimi"] {
        let status = Command::new(lain_bin())
            .args(["agents", "install", "--scope", "user", id])
            .envs(env_overrides.iter().copied())
            .status().expect("install");
        assert!(status.success(), "install {id} failed");
    }

    // 3. Run `lain agents verify --all --json` and parse.
    let output = Command::new(lain_bin())
        .args(["agents", "verify", "--all", "--json"])
        .envs(env_overrides.iter().copied())
        .output().expect("verify");
    assert!(output.status.success());
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).expect("parse json");
    assert!(rows.iter().any(|r| r["id"] == "claude" && r["mcp_reachable"].as_bool() == Some(true)));
    assert!(rows.iter().any(|r| r["id"] == "kimi" && r["mcp_reachable"].as_bool() == Some(true)));

    // 4. Tear down the server.
    let _ = server.0.kill();
}

fn pick_port() -> String {
    use std::net::TcpListener;
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let p = l.local_addr().expect("addr").port();
    drop(l);
    p.to_string()
}

fn wait_for_health(port: &str) {
    let client = reqwest::blocking::Client::new();
    let url = format!("http://127.0.0.1:{port}/mcp");
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"get_health","arguments":{}}}).to_string();
    for _ in 0..100 {
        if let Ok(r) = client.post(&url).header("content-type","application/json").body(body.clone()).send() {
            if r.status().is_success() { return; }
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    panic!("server did not become healthy on port {port}");
}
