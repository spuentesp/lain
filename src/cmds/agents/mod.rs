//! Agent installation and verification

pub mod adapters;
pub mod install;
pub mod list;
pub mod manifest;
pub mod remove;
pub mod verify;

#[cfg(test)]
pub mod tests {
    use super::manifest::{load_manifest, AgentEntry};
    use std::sync::Mutex;

    // Serializes tests that mutate the process-global HOME env var so they
    // don't race each other when cargo runs the suite in parallel.
    // `pub` so other test modules (e.g. cmds::init::tests) can also serialize
    // against HOME mutations performed outside this module.
    pub static HOME_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn loader_returns_known_agents() {
        let agents = load_manifest().expect("manifest parses");
        let ids: Vec<&str> = agents.iter().map(|a| a.id.as_str()).collect();
        for required in [
            "claude", "kimi", "cursor", "continue", "windsurf",
            "cline", "codex", "omp", "antigravity", "vscode_copilot",
        ] {
            assert!(ids.contains(&required), "missing manifest row for {required}");
        }
    }

    #[test]
    fn manifest_entries_have_non_empty_ids_and_commands() {
        let agents = load_manifest().expect("manifest parses");
        for a in &agents {
            assert!(!a.id.is_empty());
            assert!(!a.command.is_empty() || a.transport == "http");
        }
    }

    #[test]
    fn expand_home_tilde() {
        use crate::cmds::agents::adapters::expand_home;
        let p = expand_home("~/foo");
        assert!(p.to_string_lossy().ends_with("/foo"));
    }

    #[test]
    fn render_args_substitutes_workspace() {
        use crate::cmds::agents::adapters::render_args;
        let out = render_args(
            &["--workspace".into(), "{{workspace}}".into(), "--transport".into(), "stdio".into()],
            "/abs/path",
        );
        assert_eq!(out, vec!["--workspace", "/abs/path", "--transport", "stdio"]);
    }

    #[test]
    fn claude_round_trip_under_temp_home() {
        use crate::cmds::agents::adapters::{adapter_for, InstallScope};
        use crate::cmds::agents::manifest::load_manifest;
        use std::env;
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev = env::var_os("HOME");
        env::set_var("HOME", tmp.path());
        let agents = load_manifest().expect("manifest");
        let entry = agents.iter().find(|a| a.id == "claude").expect("claude row");
        let adapter = adapter_for("claude").expect("claude adapter");
        adapter.install(entry, InstallScope::User).expect("install");
        let written = std::fs::read_to_string(tmp.path().join(".claude/settings.json")).expect("read");
        assert!(written.contains("\"mcpServers\""));
        assert!(written.contains("\"lain\""));
        if let Some(prev) = prev { env::set_var("HOME", prev); }
    }

    #[test]
    fn run_install_all_writes_per_id() {
        use crate::cmds::agents::adapters::InstallScope;
        use crate::cmds::agents::install::run_install;
        use std::env;
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev = env::var_os("HOME");
        env::set_var("HOME", tmp.path());
        // Limit to claude + kimi for speed.
        let ids = ["claude", "kimi"];
        for id in ids {
            run_install(Some(id), false, InstallScope::User).expect("install");
        }
        assert!(tmp.path().join(".claude/settings.json").exists());
        assert!(tmp.path().join(".kimi-code/plugins/managed/lain/kimi.plugin.json").exists());
        if let Some(prev) = prev { env::set_var("HOME", prev); }
    }

    #[test]
    fn list_returns_known_ids() {
        use crate::cmds::agents::list::run_list;
        run_list().expect("list runs");
    }
}
