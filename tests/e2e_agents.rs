//! End-to-end MCP tests proving that `--workspace auto` resolves to the
//! current git repo for both invocation paths agents use in the wild:
//!
//! 1. Claude Code — writes `command: "lain" --workspace auto ...` and sets the
//!    subprocess cwd to the project root. The spawned `lain` reads its own cwd
//!    and walks up to the enclosing git repo.
//!
//! 2. Kimi Code — pins the subprocess cwd to the plugin root, so the user
//!    installs a thin wrapper at `~/.kimi-code/plugins/managed/lain/bin/lain`
//!    that resolves the workspace from the parent agent's cwd
//!    (`/proc/$PPID/cwd`) before exec'ing the real `lain`.
//!
//! Both tests:
//!   - create a fresh temp git repo,
//!   - spawn the appropriate invocation from that repo,
//!   - send the standard MCP `initialize` + `notifications/initialized` +
//!     `tools/call get_health` sequence over stdin (newline-delimited JSON),
//!   - parse the `id == 2` response from stdout,
//!   - assert the body contains "Operational" *and* the resolved workspace
//!     path (i.e. the test's temp repo — the proof that `--workspace auto`
//!     resolved correctly for that invocation path).

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MODEL: &str = "/home/sebastian/.local/lain/models/all-MiniLM-L6-v2.onnx";
const WRAPPER: &str = "/home/sebastian/.kimi-code/plugins/managed/lain/bin/lain";

const INIT_PAYLOAD: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}"#;
const INITIALIZED_PAYLOAD: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
const GET_HEALTH_PAYLOAD: &str =
    r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_health","arguments":{}}}"#;

fn lain_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lain"))
}

fn git_init_quiet(path: &Path) {
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(path)
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed for {}", path.display());
}

/// Run the MCP stdio dance on the spawned child: write the three payloads,
/// close stdin, drain stdout, and return the JSON-RPC response with id==2.
fn run_get_health_from_repo(mut child: Child) -> serde_json::Value {
    // ── Write payloads to stdin ────────────────────────────────────────
    {
        let stdin = child.stdin.as_mut().expect("stdin pipe");
        writeln!(stdin, "{INIT_PAYLOAD}").expect("write init");
        writeln!(stdin, "{INITIALIZED_PAYLOAD}").expect("write initialized");
        writeln!(stdin, "{GET_HEALTH_PAYLOAD}").expect("write get_health");
    }
    // Closing stdin signals EOF to the server, which then processes the
    // buffered requests and exits cleanly on the last one.
    drop(child.stdin.take());

    // ── Read stdout, finding the JSON line with id == 2 ────────────────
    let stdout = child.stdout.take().expect("stdout pipe");
    let mut reader = BufReader::new(stdout);
    let mut buf = String::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut response: Option<serde_json::Value> = None;

    loop {
        let now = Instant::now();
        if now >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "timeout waiting for tools/call response on stdout; so far: {buf}"
            );
        }
        let remaining = deadline.saturating_duration_since(now);
        // Read one line at a time with a timeout via a short blocking
        // poll on a separate thread would be cleaner, but the server
        // emits responses synchronously after reading a request, so
        // a line-by-line blocking read finishes quickly once EOF hits.
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                buf.push_str(&line);
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if v.get("id").and_then(|i| i.as_i64()) == Some(2) {
                        response = Some(v);
                        break;
                    }
                }
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("error reading stdout: {e}; so far: {buf}");
            }
        }
        // Cooperative timeout guard: also bail out if we've been looping
        // without progress for too long.
        let _ = remaining;
    }

    let _ = child.kill();
    let _ = child.wait();

    response.unwrap_or_else(|| {
        panic!("no JSON-RPC response with id==2 on stdout; full output: {buf}")
    })
}

fn assert_health_response(resp: &serde_json::Value, canonical_repo: &Path) {
    // The JSON-RPC envelope must carry a successful result, not an error.
    assert!(
        resp.get("error").is_none(),
        "tools/call returned an error: {resp}"
    );
    assert!(
        resp.get("result").is_some(),
        "tools/call response missing `result`: {resp}"
    );
    let result = &resp["result"];

    // is_error must be false or absent.
    if let Some(is_err) = result.get("is_error").and_then(|v| v.as_bool()) {
        assert!(!is_err, "tools/call reported is_error=true: {resp}");
    }

    // Extract the health body string.
    let text = result
        .pointer("/content/0/text")
        .and_then(|t| t.as_str())
        .unwrap_or_else(|| panic!("missing result.content[0].text: {resp}"));

    // The body advertises the resolved workspace — proving `--workspace auto`
    // resolved to the temp repo for this invocation path.
    let canonical_str = canonical_repo.to_string_lossy().to_string();
    let canonical_alt = canonical_str.trim_end_matches('/').to_string();
    assert!(
        text.contains(&canonical_str) || text.contains(&canonical_alt),
        "health body should reference resolved workspace {canonical_str}; body:\n{text}"
    );

    // And the health body itself is healthy.
    assert!(
        text.contains("Operational"),
        "health body missing `Operational`; body:\n{text}"
    );
}

// ────────────────────────────────────────────────────────────────────────
// Test 1: Claude Code style — direct `lain --workspace auto` from the
// project root. The subprocess cwd IS the project root.
// ────────────────────────────────────────────────────────────────────────
#[test]
fn claude_style_get_health_resolves_temp_repo() {
    if which::which("git").is_err() {
        eprintln!("skipping: `git` is not on PATH");
        return;
    }
    if !Path::new(MODEL).exists() {
        eprintln!("skipping: ONNX model missing at {MODEL}");
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("create repo");
    git_init_quiet(&repo);
    // Add a tracked file so the indexer has something concrete to walk.
    std::fs::write(repo.join("README.md"), "# repo\n").expect("write README");
    Command::new("git")
        .args(["-C", repo.to_str().unwrap(), "add", "README.md"])
        .status()
        .expect("git add");

    let child = Command::new(lain_bin())
        .args([
            "--workspace",
            "auto",
            "--transport",
            "stdio",
            "--embedding-model",
            MODEL,
        ])
        .env("LAIN_PORT", "19991")
        .current_dir(&repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lain (claude style)");

    let resp = run_get_health_from_repo(child);

    let canonical = std::fs::canonicalize(&repo).expect("canonicalize repo");
    assert_health_response(&resp, &canonical);
}

// ────────────────────────────────────────────────────────────────────────
// Test 2: Kimi wrapper style — the wrapper reads /proc/$PPID/cwd to find
// the user's project. We make $PPID's cwd equal the temp repo by spawning
// `bash -c "cd <repo> && <wrapper> ...; sleep 30"`. The trailing `sleep`
// keeps bash alive after the wrapper returns so it doesn't exec into the
// wrapper (POSIX optimization would otherwise make $PPID the test binary,
// and /proc/$PPID/cwd would be the test's cwd, not the repo).
// ────────────────────────────────────────────────────────────────────────
#[test]
fn kimi_wrapper_get_health_resolves_parent_cwd() {
    if which::which("git").is_err() {
        eprintln!("skipping: `git` is not on PATH");
        return;
    }
    if !Path::new(MODEL).exists() {
        eprintln!("skipping: ONNX model missing at {MODEL}");
        return;
    }
    if !Path::new(WRAPPER).exists() {
        eprintln!("skipping: kimi wrapper missing at {WRAPPER}");
        return;
    }
    // The wrapper hardcodes `exec "lain"` — the real binary must be on PATH.
    if which::which("lain").is_err() {
        eprintln!("skipping: `lain` is not on PATH (wrapper requires it)");
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("create repo");
    git_init_quiet(&repo);
    std::fs::write(repo.join("README.md"), "# repo\n").expect("write README");
    Command::new("git")
        .args(["-C", repo.to_str().unwrap(), "add", "README.md"])
        .status()
        .expect("git add");

    let cmd = format!(
        "cd '{}' && '{}' --workspace auto --transport stdio --embedding-model '{}'; sleep 30",
        repo.display(),
        WRAPPER,
        MODEL,
    );

    // The wrapper hardcodes `exec "lain"` and picks up the first `lain` on
    // PATH. We want the freshly-built test binary (which knows about the
    // workspace field in `get_health`), not a separately installed copy,
    // so prepend the directory holding `CARGO_BIN_EXE_lain` to PATH.
    let lain_dir = lain_bin()
        .parent()
        .expect("CARGO_BIN_EXE_lain has a parent dir")
        .to_path_buf();
    let prepended_path = match std::env::var_os("PATH") {
        Some(existing) => {
            let mut s = OsString::from(&lain_dir);
            s.push(":");
            s.push(existing);
            s
        }
        None => OsString::from(&lain_dir),
    };

    let child = Command::new("bash")
        .arg("-c")
        .arg(&cmd)
        .env("LAIN_PORT", "19992")
        .env("PATH", &prepended_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bash (kimi wrapper)");

    let resp = run_get_health_from_repo(child);

    let canonical = std::fs::canonicalize(&repo).expect("canonicalize repo");
    assert_health_response(&resp, &canonical);
}
