//! Verifies that `--workspace auto` resolves to the git repo discovered
//! from the current working directory.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn lain_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lain"))
}

#[test]
fn auto_workspace_resolves_to_git_root() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&repo)
        .status()
        .expect("git init");
    assert!(status.success());

    let mut child = Command::new(lain_bin())
        .args(["--workspace", "auto", "--transport", "stdio", "--log-level", "info"])
        .env("LAIN_PORT", "19999")
        .current_dir(&repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lain");

    let mut stderr = String::new();
    let start = Instant::now();
    let deadline = Duration::from_secs(20);
    use std::io::{BufRead, BufReader};
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

    assert!(
        stderr.contains("Serving repo"),
        "stderr should advertise the resolved workspace; got: {stderr}"
    );
    assert!(
        stderr.contains(repo.to_str().unwrap()),
        "stderr should include the resolved path; got: {stderr}"
    );
}
