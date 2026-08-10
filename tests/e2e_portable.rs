//! End-to-end portability test for the `--workspace auto` feature.
//!
//! Advertised behavior: a single `lain init --agent <agent>` install works
//! for any git repository. The MCP args written to the agent's config
//! contain `--workspace auto`; at server startup Lain resolves the
//! workspace from the agent's current working directory.
//!
//! This test proves the full user-facing claim by:
//!   1. Running `lain init --agent claude --yes` from temp repo A.
//!   2. Reading the resulting `~/.claude/settings.json` and asserting
//!      that the MCP args contain `--workspace auto` (not a hardcoded
//!      absolute path).
//!   3. Spawning the same `lain` binary the agent would invoke, but with
//!      `current_dir = temp repo B`, and asserting that stderr contains
//!      `Serving repo` followed by repo B's path (not repo A's).

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn lain_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lain"))
}

fn git_init_quiet(path: &std::path::Path) {
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(path)
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed for {}", path.display());
}

#[test]
fn lain_init_in_repo_a_then_run_from_repo_b_serves_repo_b() {
    // Isolated environment so we do not touch the user's real registry
    // or home directory. The tempdir is removed when `home` drops.
    let home = tempfile::tempdir().expect("tempdir home");
    let repo_a = home.path().join("repo-a");
    let repo_b = home.path().join("repo-b");
    std::fs::create_dir_all(&repo_a).expect("create repo-a");
    std::fs::create_dir_all(&repo_b).expect("create repo-b");
    git_init_quiet(&repo_a);
    git_init_quiet(&repo_b);

    // Make repo A look like a real project by adding a tracked file;
    // the indexer happily skips empty repos, but a file exercises the
    // same code path a normal user would hit.
    std::fs::write(
        repo_a.join("README.md"),
        "# repo A\n",
    )
    .expect("write repo-a README");
    Command::new("git")
        .args(["-C", repo_a.to_str().unwrap(), "add", "README.md"])
        .status()
    .expect("git add");
    std::fs::write(
        repo_b.join("README.md"),
        "# repo B\n",
    )
    .expect("write repo-b README");
    Command::new("git")
        .args(["-C", repo_b.to_str().unwrap(), "add", "README.md"])
        .status()
    .expect("git add");

    let home_str = home.path().to_string_lossy().to_string();
    let repo_a_str = repo_a.to_string_lossy().to_string();

    // ── Step 1: `lain init --agent claude --yes` from repo A. ────────
    let init_status = Command::new(lain_bin())
        .args(["init", "--agent", "claude", "--yes"])
        .args(["--workspace", &repo_a_str])
        .env("HOME", &home_str)
        .env("XDG_CONFIG_HOME", &home_str)
        .env("LAIN_PORT", "19999")
        .current_dir(&repo_a)
        .status()
        .expect("spawn lain init");
    assert!(
        init_status.success(),
        "lain init failed with {init_status:?}"
    );

    // ── Step 2: read ~/.claude/settings.json and verify auto. ────────
    let settings_path = home.path().join(".claude/settings.json");
    let body = std::fs::read_to_string(&settings_path)
        .expect("read ~/.claude/settings.json");
    let json: serde_json::Value =
        serde_json::from_str(&body).expect("parse settings.json");
    let args = json
        .pointer("/mcpServers/lain/args")
        .expect("mcpServers.lain.args present")
        .as_array()
        .expect("args is an array");
    let slice: Vec<String> = args
        .iter()
        .map(|v| v.as_str().expect("arg is a string").to_string())
        .collect();
    assert!(
        slice.windows(2).any(|w| w == ["--workspace", "auto"]),
        "expected --workspace auto in installed args, got: {slice:?}"
    );
    assert!(
        !slice.iter().any(|a| a == &repo_a_str),
        "args must not contain repo A's path; got: {slice:?}"
    );

    // ── Step 3: spawn the binary as the agent would, from repo B. ────
    let mut child = Command::new(lain_bin())
        .args(["--workspace", "auto", "--transport", "stdio", "--log-level", "info"])
        .env("HOME", &home_str)
        .env("XDG_CONFIG_HOME", &home_str)
        .env("LAIN_PORT", "19998")
        .current_dir(&repo_b)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lain server");

    use std::io::{BufRead, BufReader};
    let mut stderr = String::new();
    let start = Instant::now();
    let deadline = Duration::from_secs(30);
    let mut pipe = BufReader::new(child.stderr.take().expect("stderr pipe")).lines();
    loop {
        match pipe.next() {
            Some(Ok(line)) => {
                stderr.push_str(&line);
                stderr.push('\n');
                if stderr.contains("Serving repo") {
                    break;
                }
            }
            Some(Err(_)) | None => break,
        }
        if start.elapsed() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("timeout waiting for Serving repo (stderr so far: {stderr})");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();

    // ── Verify: the binary served repo B, not repo A. ────────────────
    let canonical_b =
        std::fs::canonicalize(&repo_b).expect("canonicalize repo B");
    let canonical_b_str = canonical_b.to_string_lossy().to_string();
    let canonical_b_alt = canonical_b_str.trim_end_matches('/').to_string();

    assert!(
        stderr.contains("Serving repo"),
        "stderr should advertise the resolved workspace; got: {stderr}"
    );
    assert!(
        stderr.contains(&canonical_b_str) || stderr.contains(&canonical_b_alt),
        "Serving repo should reference repo B ({canonical_b_str}); got: {stderr}"
    );
    assert!(
        !stderr.contains(&repo_a_str) || !stderr.lines()
            .any(|l| l.contains("Serving repo") && l.contains(&repo_a_str)),
        "Serving repo must not reference repo A ({repo_a_str}); got: {stderr}"
    );
}
