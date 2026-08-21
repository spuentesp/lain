//! End-to-end regression test for `lain oneshot`.
//!
//! Bug: oneshot closed the child's stdin immediately after writing
//! initialize + tools/call. EOF makes the MCP SDK start shutting down
//! the stdio transport; when a startup re-index is in flight (fresh or
//! stale graph) the single-threaded `lain mcp` runtime is busy and the
//! in-flight tools/call loses the race against the shutdown — the
//! response never arrives. The client's blocking `read()` then waited
//! forever, because the deadline was only checked *after* read
//! returned. Net effect: `lain oneshot` hung for the full timeout
//! (or indefinitely) exactly on the first run against a repo — the
//! worst possible first-run experience.
//!
//! Fix under test: the client keeps stdin open until the tools/call
//! response arrives and enforces the deadline with `recv_timeout` on a
//! reader thread instead of a blocking read.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Build a temp git repo with enough Rust files that the startup
/// re-index takes measurably longer than a tools/call dispatch.
/// Returns the TempDir (kept alive by the caller) and its path.
fn make_repo(files: usize) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    for i in 0..files {
        let mut body = String::new();
        for j in 0..20 {
            body.push_str(&format!(
                "pub fn fn_{i}_{j}(x: u32) -> u32 {{ helper_{i}(x) + {j} }}\n"
            ));
        }
        body.push_str(&format!("fn helper_{i}(x: u32) -> u32 {{ x + {i} }}\n"));
        std::fs::write(root.join("src").join(format!("mod{i}.rs")), body).unwrap();
    }
    std::fs::write(root.join("src/lib.rs"), "// test repo\n").unwrap();

    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {:?} failed", args);
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init"]);
    (tmp, root)
}

/// Spawn `lain oneshot` against `workspace` with an isolated state dir
/// and wait up to `budget` for it to exit, killing it past the budget.
/// Returns (exit_success, stdout, elapsed).
fn run_oneshot(workspace: &std::path::Path, budget: Duration) -> (bool, String, Duration) {
    let state = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lain"))
        .args([
            "oneshot",
            "--workspace",
            workspace.to_str().unwrap(),
            "find_anchors",
        ])
        .env("XDG_STATE_HOME", state.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if start.elapsed() > budget {
            let _ = child.kill();
            let _ = child.wait();
            return (false, String::from("<killed: exceeded budget>"), start.elapsed());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut out)
        .unwrap();
    (status.success(), out, start.elapsed())
}

/// First-ever run against a repo (cold graph, re-index in flight) must
/// still return within the oneshot deadline — the pre-fix code hung
/// here because the tools/call raced the EOF-triggered shutdown.
///
/// Budget is generous (75s > default LAIN_ONESHOT_TIMEOUT=60s) so a
/// genuine-but-slow success still passes; the pre-fix hang either
/// exceeded the internal 60s deadline (error exit) or blocked forever
/// (killed at 75s).
#[test]
fn oneshot_returns_on_cold_graph() {
    let (_tmp, root) = make_repo(30);
    let (ok, out, elapsed) = run_oneshot(&root, Duration::from_secs(75));
    assert!(
        ok,
        "oneshot failed on cold graph after {:?}; stdout:\n{}",
        elapsed, out
    );
    assert!(
        out.contains("anchors") || out.contains("No anchors"),
        "unexpected oneshot output:\n{}",
        out
    );
}

/// Second run with a warm graph must be fast (<20s even on slow CI).
#[test]
fn oneshot_warm_graph_is_fast() {
    let (_tmp, root) = make_repo(5);
    let (ok, _, _) = run_oneshot(&root, Duration::from_secs(75));
    assert!(ok, "warm-up run failed");
    let (ok, out, elapsed) = run_oneshot(&root, Duration::from_secs(30));
    assert!(ok, "warm run failed: {}", out);
    assert!(
        elapsed < Duration::from_secs(20),
        "warm oneshot took {:?}, expected <20s",
        elapsed
    );
}

/// `lain oneshot` from inside the repo (no --workspace) resolves the
/// workspace by walking up to `.git`.
#[test]
fn oneshot_discovers_workspace_from_cwd() {
    let (_tmp, root) = make_repo(3);
    let state = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lain"))
        .args(["oneshot", "find_anchors"])
        .current_dir(root.join("src"))
        .env("XDG_STATE_HOME", state.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if start.elapsed() > Duration::from_secs(75) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("oneshot (cwd discovery) exceeded 75s budget — hung");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(status.success(), "exit: {:?}", status);
}
