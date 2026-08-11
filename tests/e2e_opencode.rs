//! End-to-end test for `lain init --agent opencode`.
//!
//! Runs the real binary in a temp git repo and verifies the produced
//! `opencode.json` matches the schema at
//! <https://opencode.ai/docs/mcp-servers/>.

use std::path::PathBuf;
use std::process::Command;

/// `Drop`-based HOME restore. Captures the previous value on `set`,
/// then restores it on scope exit — including the panic path. Without
/// this, an assertion panic between `set_var("HOME", tmp)` and the
/// explicit restore would leak the tempdir HOME into every subsequent
/// test that runs in this process.
struct HomeGuard(Option<String>);
impl HomeGuard {
    fn set(path: &std::path::Path) -> Self {
        let prev = std::env::var("HOME").ok();
        std::env::set_var("HOME", path);
        Self(prev)
    }
}
impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}

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
fn lain_init_opencode_writes_verified_opencode_json_and_agents_md() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("create repo");
    git_init_quiet(&repo);

    let status = Command::new(lain_bin())
        .args(["init", "--agent", "opencode", "--yes"])
        .args(["--workspace", repo.to_str().unwrap()])
        .current_dir(&repo)
        .status()
        .expect("spawn lain init");
    assert!(status.success(), "lain init exited with {status:?}");

    let body = std::fs::read_to_string(repo.join("opencode.json")).expect("read opencode.json");
    let doc: serde_json::Value = serde_json::from_str(&body).expect("parse opencode.json");

    let lain = doc.pointer("/mcp/lain").expect("mcp.lain present");
    assert_eq!(lain["type"], "local");
    let cmd = lain["command"].as_array().expect("command is a JSON array");
    let cmd: Vec<String> = cmd.iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert_eq!(cmd.first().map(String::as_str), Some("lain"),
        "command[0] must be the bare name `lain`, got {:?}", cmd.first());
    assert!(cmd.windows(2).any(|w| w == ["--workspace", "auto"]));
    assert!(cmd.windows(2).any(|w| w == ["--transport", "stdio"]));
    assert_eq!(lain["enabled"], true);
    assert_eq!(lain["timeout"], 30000);

    let agents = repo.join("AGENTS.md");
    assert!(agents.exists(), "AGENTS.md not written to project root");
    let agents_body = std::fs::read_to_string(&agents).expect("read AGENTS.md");
    assert!(agents_body.contains("When to use lain"));
    assert!(agents_body.contains("find_anchors"));
}

#[test]
fn lain_init_opencode_scope_user_writes_global_only() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("create repo");
    git_init_quiet(&repo);

    // `HomeGuard` restores the prior HOME on scope exit (including the
    // panic path). Binding it to a local keeps restoration tied to
    // this test's frame.
    let _home_guard = HomeGuard::set(tmp.path());

    let status = Command::new(lain_bin())
        .args(["init", "--agent", "opencode", "--yes", "--scope", "user"])
        .args(["--workspace", repo.to_str().unwrap()])
        .current_dir(&repo)
        .status()
        .expect("spawn lain init");
    assert!(status.success(), "lain init exited with {status:?}");

    let global = tmp.path().join(".config/opencode/opencode.json");
    assert!(global.exists(), "user-scope must write ~/.config/opencode/opencode.json");
    assert!(!repo.join("opencode.json").exists(),
        "user-scope must NOT write project opencode.json");
    assert!(!repo.join("AGENTS.md").exists(),
        "user-scope must NOT write AGENTS.md");
}
