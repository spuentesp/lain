//! Repository diagnosis and protocol regressions, with isolated user state.

use lain::graph::GraphDatabase;
use serde_json::Value;
use std::process::Command;
use tempfile::TempDir;

struct Fixture {
    repo: TempDir,
    home: TempDir,
}

impl Fixture {
    fn new(indexed: bool) -> Self {
        let fixture = Self {
            repo: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
        };
        let repo = git2::Repository::init(fixture.repo.path()).unwrap();
        std::fs::write(fixture.repo.path().join(".gitignore"), ".lain/\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(".gitignore")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let commit = repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        if indexed {
            std::fs::create_dir(fixture.repo.path().join(".lain")).unwrap();
            let graph = GraphDatabase::new(&fixture.repo.path().join(".lain/graph.bin")).unwrap();
            graph.set_last_commit(commit.to_string()).unwrap();
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(graph.save_to_disk())
                .unwrap();
        }
        fixture
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_lain"));
        cmd.arg("doctor")
            .current_dir(self.repo.path())
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("XDG_STATE_HOME", self.home.path().join("state"))
            .env_remove("LAIN_URL")
            .env_remove("LAIN_SERVER_URL")
            .env_remove("LAIN_EMBEDDING_MODEL")
            .env_remove("LAIN_WORKSPACE");
        cmd
    }
    fn json(&self, code: i32) -> Value {
        let out = self.command().arg("--json").output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(code),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["server_version"], env!("CARGO_PKG_VERSION"));
        value
    }
}

#[test]
fn current_snapshot_is_ready_without_optional_model_or_user_directories() {
    let fixture = Fixture::new(true);
    let before = std::fs::read(fixture.repo.path().join(".lain/graph.bin")).unwrap();
    let report = fixture.json(0);
    assert_eq!(report["agent_ready"], true);
    assert_eq!(report["transport"]["kind"], "stdio_probe");
    assert_eq!(report["transport"]["healthy"], true);
    assert!(report["transport"]["tools_count"].as_u64().unwrap() > 0);
    assert_eq!(
        report["capabilities"]["semantic_search"]["state"],
        "unavailable_optional"
    );
    assert_eq!(report["installation"]["config_dir_state"], "missing");
    assert_eq!(std::fs::read_dir(fixture.home.path()).unwrap().count(), 0);
    assert_eq!(
        std::fs::read(fixture.repo.path().join(".lain/graph.bin")).unwrap(),
        before
    );
    let human = fixture.command().output().unwrap();
    assert!(human.status.success());
    let text = String::from_utf8(human.stdout).unwrap();
    for expected in [
        "lain doctor",
        "binary version",
        "commit",
        "config dir",
        "hooks dir",
        "hook script",
        "Agent-ready: YES",
    ] {
        assert!(text.contains(expected), "{text}");
    }
}

#[test]
fn missing_index_is_unusable_and_diagnosis_does_not_create_it() {
    let fixture = Fixture::new(false);
    let report = fixture.json(2);
    assert_eq!(report["agent_ready"], false);
    assert!(report["problems"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["code"] == "graph_missing"));
    assert!(!fixture.repo.path().join(".lain").exists());
}

#[test]
fn corrupt_index_is_reported_and_preserved() {
    let fixture = Fixture::new(true);
    let graph = fixture.repo.path().join(".lain/graph.bin");
    std::fs::write(&graph, b"corrupt graph").unwrap();
    let report = fixture.json(2);
    assert!(report["problems"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["code"] == "graph_corrupt"
            && p["remediation"].as_str().unwrap().contains("backup")));
    assert_eq!(std::fs::read(graph).unwrap(), b"corrupt graph");
}

#[test]
fn dirty_snapshot_is_usable_but_degraded() {
    let fixture = Fixture::new(true);
    std::fs::write(fixture.repo.path().join("new.rs"), "fn example() {}\n").unwrap();
    let report = fixture.json(1);
    assert_eq!(report["agent_ready"], true);
    assert_eq!(report["capabilities"]["symbols"]["state"], "stale_usable");
    assert_eq!(report["repository"]["working_tree_dirty"], true);
    assert_eq!(report["repository"]["working_tree_overlay"], false);
}

#[test]
fn missing_repository_is_structured_even_when_parent_is_a_repository() {
    let fixture = Fixture::new(false);
    let out = fixture
        .command()
        .args(["--json", "--workspace"])
        .arg(fixture.home.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["repository"], Value::Null);
    assert_eq!(report["problems"][0]["code"], "repository_not_found");
}

#[test]
fn diagnosis_keeps_cached_sessions() {
    let fixture = Fixture::new(true);
    let hooks = fixture.home.path().join("config/lain/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let session = hooks.join("old.session");
    std::fs::write(&session, "old cached session").unwrap();
    let file = std::fs::File::open(&session).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
        .unwrap();
    fixture.json(0);
    assert_eq!(
        std::fs::read_to_string(session).unwrap(),
        "old cached session"
    );
}

#[test]
fn capability_and_status_commands_project_the_same_readiness() {
    let fixture = Fixture::new(true);
    let doctor = fixture.json(0);
    for command in ["capabilities", "status"] {
        let output = Command::new(env!("CARGO_BIN_EXE_lain"))
            .args([command, "--json", "--workspace"])
            .arg(fixture.repo.path())
            .env("HOME", fixture.home.path())
            .env("LAIN_CACHE_DIR", fixture.home.path().join("cache"))
            .env_remove("LAIN_URL")
            .env_remove("LAIN_SERVER_URL")
            .env_remove("LAIN_EMBEDDING_MODEL")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["schema_version"], doctor["schema_version"]);
        assert_eq!(value["server_version"], doctor["server_version"]);
        assert_eq!(value["capabilities"], doctor["capabilities"]);
        if command == "status" {
            assert_eq!(value["agent_ready"], doctor["agent_ready"]);
            assert!(value.get("indexing").is_some());
        }
    }
}

/// Regression test for the doctor `tools/list` happy path.
///
/// The pre-fix code sent `{"method": "tools/call", "params": {"name": "tools/list"}}`
/// instead of `{"method": "tools/list", "params": {}}`. The unit test in
/// `cli/mcp_client.rs::build_envelope_uses_supplied_method_at_top_level`
/// guards the envelope shape; this test guards the end-to-end behavior
/// against a real running server. Without it, a regression in the
/// envelope (or in `emit_tools_list_check` itself) could pass all
/// existing tests and only show up when an operator runs doctor against
/// a live federation.
#[test]
fn lain_doctor_reports_live_mcp_surface_against_real_server() {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    // Find a free port by binding + releasing. There is a small race
    // window where someone else could grab the port between the
    // release and `lain server`'s bind, but in practice on a CI
    // runner with no other listeners this is reliable.
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };

    // Minimal `repos.yaml` so the server has something to read. The
    // federation is empty — we only need the server's MCP surface to
    // be alive, not to have real repos.
    let project = tempfile::tempdir().unwrap();
    let data_dir = project.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(
        project.path().join("repos.yaml"),
        format!("data_dir: {}\nrepos: []\n", data_dir.display()),
    )
    .unwrap();

    let state = tempfile::tempdir().unwrap();
    let mut server = Command::new(env!("CARGO_BIN_EXE_lain"))
        .args([
            "server",
            "--transport",
            "http",
            "--port",
            &port.to_string(),
            "--config",
            project.path().join("repos.yaml").to_str().unwrap(),
        ])
        .env("XDG_STATE_HOME", state.path())
        .env("LAIN_JOB_STORE", state.path().join("jobs.json"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn lain server");

    // Poll `/health` until it returns 200. A plain TCP connect isn't
    // enough — the listener accepts before the server is fully
    // initialized, and `tools/list` would race against initialization.
    let health_url_host = format!("127.0.0.1:{port}");
    let start = Instant::now();
    let ready = loop {
        if start.elapsed() > Duration::from_secs(15) {
            let _ = server.kill();
            let _ = server.wait();
            panic!("server did not become healthy within 15s");
        }
        let attempt: std::io::Result<()> = (|| {
            let mut stream = TcpStream::connect(&health_url_host)?;
            let req = format!(
                "GET /health HTTP/1.1\r\nHost: {health_url_host}\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(req.as_bytes())?;
            let mut response = String::new();
            stream.read_to_string(&mut response)?;
            if response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200") {
                Ok(())
            } else {
                Err(std::io::Error::other(format!("not 200: {response}")))
            }
        })();
        if attempt.is_ok() {
            break true;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(ready, "server never returned 200 from /health");

    // Run doctor with `LAIN_URL` pointing at the live server.
    let fixture = Fixture::new(true);
    let doctor_out = fixture
        .command()
        .env("LAIN_URL", format!("http://127.0.0.1:{port}"))
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("run doctor");

    // Cleanup: kill the server regardless of the doctor outcome.
    let _ = server.kill();
    let _ = server.wait();

    let stdout = String::from_utf8_lossy(&doctor_out.stdout);
    assert!(
        doctor_out.status.success(),
        "doctor failed (status {:?}):\n{stdout}",
        doctor_out.status.code()
    );
    assert!(
        stdout.contains("MCP surface live: tools/list advertises"),
        "doctor did not report live MCP surface; stdout:\n{stdout}"
    );
    // Negative assertion: the failure-mode line for an empty surface
    // must not appear. (The exact count wording is "advertises N tools";
    // we don't pin N because it changes as the tool surface evolves.)
    assert!(
        !stdout.contains("MCP surface empty"),
        "doctor reported MCP surface empty; stdout:\n{stdout}"
    );
}
