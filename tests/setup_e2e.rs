//! End-to-end tests for `lain setup` (AGENT_UX_ROADMAP.md Milestone 2).
//!
//! Only exercises paths with no side effects outside the fixture's own
//! tempdir: the `generic` adapter (writes `.mcp.json` inside the
//! fixture) and `--print-config`/`--dry-run`. The `claude-code` adapter
//! shells out to the real `claude` CLI's `mcp add`/`remove`, which
//! would mutate a developer's actual `~/.claude.json` — never exercised
//! here even if `claude` happens to be on the test runner's `PATH`.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

fn build_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"setup_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn hello() {}\n").unwrap();

    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "setup-test@lain"]);
    git(&["config", "user.name", "setup-test"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "fixture"]);
    dir
}

fn lain_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lain"))
}

#[test]
fn print_config_emits_valid_mcp_json_and_writes_nothing() {
    let fixture = build_fixture();
    let out = Command::new(lain_bin())
        .args(["setup", "--workspace"])
        .arg(fixture.path())
        .args(["--agent", "generic", "--print-config", "--no-model"])
        .output()
        .expect("run lain setup");
    assert!(
        out.status.success(),
        "setup --print-config must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("--print-config must print valid JSON: {e}\nstdout:\n{stdout}"));
    assert_eq!(
        value["mcpServers"]["lain"]["args"],
        serde_json::json!(["mcp"])
    );
    assert!(value["mcpServers"]["lain"]["command"]
        .as_str()
        .unwrap()
        .contains("lain"));
    assert!(
        !fixture.path().join(".mcp.json").exists(),
        "--print-config must not write .mcp.json"
    );
}

#[test]
fn dry_run_reports_would_configure_and_writes_nothing() {
    let fixture = build_fixture();
    let out = Command::new(lain_bin())
        .args(["setup", "--workspace"])
        .arg(fixture.path())
        .args(["--agent", "generic", "--dry-run", "--no-model", "--json"])
        .output()
        .expect("run lain setup");
    assert!(out.status.success(), "dry-run setup must succeed");
    let value: Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(value["configuration"]["state"], "would_configure");
    assert!(!fixture.path().join(".mcp.json").exists());
}

/// Full run against a real fixture: writes `.mcp.json`, then performs
/// the real `initialize` + `tools/list` verification round trip against
/// the exact configured command. This is the roadmap's "final
/// verification starts LAIN and performs an MCP initialize/tools-list
/// round trip" — proving setup doesn't just write a plausible-looking
/// config, but one that actually works. Milestone 4's backgrounded
/// startup means this succeeds immediately even though the graph has
/// never been indexed here.
#[test]
fn generic_setup_writes_config_and_verifies_successfully() {
    let fixture = build_fixture();
    let config_path = fixture.path().join(".mcp.json");

    let out = Command::new(lain_bin())
        .args(["setup", "--workspace"])
        .arg(fixture.path())
        .args(["--agent", "generic", "--no-model", "--json"])
        .output()
        .expect("run lain setup");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("setup must print valid JSON: {e}\nstdout:\n{stdout}"));

    assert_eq!(value["configuration"]["state"], "configured");
    assert_eq!(
        value["verification"]["healthy"], true,
        "verification must succeed against the just-written config: {value}"
    );
    assert!(
        value["verification"]["tools_count"].as_u64().unwrap_or(0) > 0,
        "tools/list must report a non-empty tool surface: {value}"
    );
    assert!(config_path.is_file(), ".mcp.json must exist after setup");
    assert!(
        out.status.success(),
        "setup must exit 0 when configured and verified: {value}"
    );

    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(
        written["mcpServers"]["lain"]["args"],
        serde_json::json!(["mcp"])
    );
}

/// Re-running setup must update the existing entry in place (never a
/// second "lain" key, never a growing pile of duplicate servers) and
/// must back up the previous config rather than silently discarding it.
#[test]
fn rerunning_setup_is_idempotent_and_backs_up() {
    let fixture = build_fixture();
    let config_path = fixture.path().join(".mcp.json");
    let run = || {
        Command::new(lain_bin())
            .args(["setup", "--workspace"])
            .arg(fixture.path())
            .args(["--agent", "generic", "--no-model", "--json"])
            .output()
            .expect("run lain setup")
    };

    let first = run();
    assert!(first.status.success(), "first setup run must succeed");
    let second = run();
    assert!(second.status.success(), "second setup run must succeed");

    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(
        written["mcpServers"].as_object().unwrap().len(),
        1,
        "re-running setup must not duplicate the lain entry: {written}"
    );

    let backups: Vec<_> = std::fs::read_dir(fixture.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".mcp.json.bak-"))
        .collect();
    assert_eq!(
        backups.len(),
        1,
        "exactly one backup expected after one re-run"
    );
}

#[test]
fn unknown_agent_fails_clearly_without_writing_anything() {
    let fixture = build_fixture();
    let out = Command::new(lain_bin())
        .args(["setup", "--workspace"])
        .arg(fixture.path())
        .args(["--agent", "not-a-real-agent", "--no-model"])
        .output()
        .expect("run lain setup");
    assert!(
        !out.status.success(),
        "an unknown --agent value must fail, not silently default"
    );
    assert!(!fixture.path().join(".mcp.json").exists());
}
