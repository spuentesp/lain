//! Capability packages — the skill layer, exercised over the wire.

#[path = "../common/mod.rs"]
#[allow(clippy::duplicate_mod)]
mod common;
use common::{boot_single_repo_in_dir, git_init_committed, jsonrpc, tools_call_envelope};
use serde_json::json;

/// Count the tools the server advertises right now. This is the
/// number that `load_package` is supposed to grow — the whole point
/// of packages is that they show up in `tools/list` like a skill
/// becoming available.
fn tools_list_count(host: &str) -> usize {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}).to_string();
    let resp = jsonrpc(host, &body);
    resp.pointer("/result/tools")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or_else(|| panic!("tools/list must return tools: {resp}"))
}

/// `list_packages` renders the menu; `load_package` opts one in and
/// `tools/list` grows, which is what makes packages feel like skills
/// rather than configuration. An unknown package fails helpfully.
#[test]
fn load_package_grows_tools_list_over_the_wire() {
    let project = tempfile::tempdir().expect("tempdir");
    let root = project.path().to_path_buf();
    let repo_dir = root.join("repo");
    std::fs::create_dir_all(repo_dir.join("src")).unwrap();
    std::fs::write(
        repo_dir.join("src/lib.rs"),
        "pub fn helper() -> u32 { 1 }\n\npub fn caller() -> u32 { helper() }\n",
    )
    .unwrap();
    git_init_committed(&repo_dir);
    let repos_yaml_path = root.join("repos.yaml");
    std::fs::write(
        &repos_yaml_path,
        format!(
            "data_dir: {}\nrepos:\n  - id: repo\n    source: {{ type: workspace_dir, path: {} }}\n",
            root.join("data").display(),
            repo_dir.display()
        ),
    )
    .unwrap();

    let (host, _guard) = boot_single_repo_in_dir(
        project.path(),
        &repo_dir,
        &repos_yaml_path,
        &["helper", "caller"],
    );

    let baseline = tools_list_count(&host);

    // The menu is the skill layer's whole pitch: what each package
    // is for, why it is off by default, and what it brings.
    let menu = tools_call_envelope(&host, "list_packages", json!({}));
    let menu_text = menu
        .pointer("/result/content/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        menu_text.contains("verify"),
        "menu lists packages: {menu_text}"
    );
    assert!(
        menu_text.contains("why_off_by_default"),
        "menu explains why packages are off: {menu_text}"
    );
    assert!(menu_text.contains("run_tests"), "menu lists the tools");

    let loaded = tools_call_envelope(&host, "load_package", json!({"package": "verify"}));
    let loaded_text = loaded
        .pointer("/result/content/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        loaded_text.contains("tools_list_changed"),
        "load_package signals a tools/list change: {loaded_text}"
    );
    assert!(
        loaded_text.contains("run_tests"),
        "load_package returns the tools it adds: {loaded_text}"
    );

    let after = tools_list_count(&host);
    assert!(
        after > baseline,
        "loading a package must grow tools/list: {baseline} -> {after}"
    );

    // An unknown package must fail helpfully, not silently.
    let bad = tools_call_envelope(&host, "load_package", json!({"package": "nonsense"}));
    assert_eq!(
        bad.pointer("/result/isError").and_then(|v| v.as_bool()),
        Some(true),
        "unknown package must error: {bad}"
    );
    let bad_text = bad
        .pointer("/result/content/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        bad_text.contains("list_packages"),
        "the error points at the menu: {bad_text}"
    );
}
