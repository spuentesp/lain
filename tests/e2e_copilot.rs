//! End-to-end test for `lain init --agent copilot`.
//!
//! Runs the real binary in a temp git repo and verifies the produced
//! `.vscode/mcp.json` matches the VS Code and GitHub Copilot MCP schema
//! at <https://code.visualstudio.com/docs/agent-customization/mcp-servers>.

use std::path::PathBuf;
use std::process::Command;

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

/// Mirrors `HomeGuard` in `src/cmds/init.rs::tests`. Each test binary
/// duplicates the type because integration tests cannot share `#[cfg(test)]`
/// items. Panic-safe HOME restore.
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

#[test]
fn lain_init_copilot_writes_verified_mcp_json_and_instructions_md() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("create repo");
    git_init_quiet(&repo);

    let status = Command::new(lain_bin())
        .args(["init", "--agent", "copilot", "--yes"])
        .args(["--workspace", repo.to_str().unwrap()])
        .current_dir(&repo)
        .status()
        .expect("spawn lain init");
    assert!(status.success(), "lain init exited with {status:?}");

    let body = std::fs::read_to_string(repo.join(".vscode/mcp.json")).expect("read .vscode/mcp.json");
    let doc: serde_json::Value = serde_json::from_str(&body).expect("parse .vscode/mcp.json");

    let lain = doc.pointer("/servers/lain").expect("servers.lain present");
    assert_eq!(lain["command"], "lain");
    let args = lain["args"].as_array().expect("args is a JSON array");
    let cmd: Vec<String> = args.iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert!(cmd.windows(2).any(|w| w == ["--workspace", "auto"]));
    assert!(cmd.windows(2).any(|w| w == ["--transport", "stdio"]));

    let instructions = repo.join(".github/copilot-instructions.md");
    assert!(instructions.exists(), "copilot-instructions.md not written");
    let body = std::fs::read_to_string(&instructions).expect("read copilot-instructions.md");
    assert!(body.contains("When to use lain"));
    assert!(body.contains("find_anchors"));
}

#[test]
fn lain_init_copilot_scope_user_writes_global_only() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("create repo");
    git_init_quiet(&repo);

    let _home_guard = HomeGuard::set(tmp.path());

    let status = Command::new(lain_bin())
        .args(["init", "--agent", "copilot", "--yes", "--scope", "user"])
        .args(["--workspace", repo.to_str().unwrap()])
        .current_dir(&repo)
        .status()
        .expect("spawn lain init");
    assert!(status.success(), "lain init exited with {status:?}");
    drop(_home_guard); // restore HOME before assertions

    let global = tmp.path().join(".copilot/mcp-config.json");
    assert!(global.exists(), "user-scope must write ~/.copilot/mcp-config.json");
    assert!(!repo.join(".vscode/mcp.json").exists(), "user-scope must NOT write project .vscode/mcp.json");
    assert!(!repo.join(".github/copilot-instructions.md").exists(), "user-scope must NOT write project awareness doc");
}
