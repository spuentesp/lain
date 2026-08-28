//! Smoke test for `lain doctor` — runs the binary, asserts exit 0
//! and that the diagnostic carries binary / git / hooks info.
//!
//! This is the wishlist item #6 ("one version of truth") verification:
//! the operator runs `lain doctor` and gets a single page that
//! confirms the binary version, the git sha it was built from,
//! and the on-disk state of the hook scripts + config + hooks dirs.

use std::process::Command;

fn lain() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lain"))
}

#[test]
fn lain_doctor_runs_and_exits_zero() {
    let out = lain().args(["doctor"]).output().expect("run doctor");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "lain doctor failed (status {:?}): {stdout}",
        out.status.code()
    );
    // Header should appear at the top.
    assert!(
        stdout.contains("lain doctor"),
        "missing header: {stdout}"
    );
    // Check 1: binary version + git sha.
    assert!(
        stdout.contains("binary") && stdout.contains("version"),
        "missing binary version line: {stdout}"
    );
    assert!(
        stdout.to_lowercase().contains("git") || stdout.contains("commit"),
        "missing git-sha info: {stdout}"
    );
}

#[test]
fn lain_doctor_mentions_hook_and_config_dirs() {
    let out = lain().args(["doctor"]).output().expect("run doctor");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "lain doctor failed: {stdout}");
    // The four filesystem-facing checks should appear in the output.
    assert!(
        stdout.contains("hook"),
        "missing hook-script check: {stdout}"
    );
    assert!(
        stdout.contains("config"),
        "missing config-dir check: {stdout}"
    );
    assert!(
        stdout.contains("hooks dir"),
        "missing hooks-dir check: {stdout}"
    );
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
    let doctor_out = Command::new(env!("CARGO_BIN_EXE_lain"))
        .args(["doctor"])
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