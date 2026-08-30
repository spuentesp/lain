//! End-to-end proving test for `get_call_sites`.
//!
//! The wishlist audit found a regression where `get_call_sites`
//! reported the *enclosing function's* definition range as a call
//! site, instead of the actual line(s) where the call appeared. The
//! current behavior scans the caller's body for the symbol name and
//! returns the real lines. This test pins that contract end-to-end:
//!
//! - The fixture has `caller()` with 6 distinct calls to `target()`
//!   on separate lines.
//! - The tool must report all 6 lines (not one 30-line span).
//! - The format is "N call(s) across M function(s)" — the multi-call
//!   variant of the heading, not the single-call "1 found" variant.

#[path = "../common/mod.rs"]
mod common;
use common::{boot_single_repo, git_init_committed, tools_call_text};

#[test]
fn get_call_sites_reports_each_distinct_call_line_not_enclosing_function() {
    let project = tempfile::tempdir().unwrap();
    let repo_dir = project.path().join("repo");
    std::fs::create_dir_all(repo_dir.join("src")).unwrap();
    std::fs::write(
        repo_dir.join("Cargo.toml"),
        "[package]\nname = \"call-sites-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    // `caller` has 6 distinct calls on 6 separate lines, plus a
    // trailing statement to keep the function ending on a
    // non-call line. Before the fix, the tool reported the entire
    // `[2, 9]` span as "1 call site". After the fix, it reports
    // each of the 6 lines.
    std::fs::write(
        repo_dir.join("src/lib.rs"),
        "pub fn target() -> u32 { 0 }\n\
         pub fn caller() -> u32 {\n    \
         let _ = target();\n    \
         let _ = target();\n    \
         let _ = target();\n    \
         let _ = target();\n    \
         let _ = target();\n    \
         let _ = target();\n    \
         0\n\
         }\n",
    )
    .unwrap();
    git_init_committed(&repo_dir);

    let data_dir = project.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let repos_yaml = format!(
        "data_dir: {}\nrepos:\n  - id: repo\n    source:\n      type: workspace_dir\n      path: {}\n",
        data_dir.display(),
        repo_dir.display(),
    );
    let repos_yaml_path = project.path().join("repos.yaml");
    std::fs::write(&repos_yaml_path, repos_yaml).unwrap();

    let (host, _guard) = boot_single_repo(
        &repo_dir,
        &repos_yaml_path,
        &["target", "caller"],
    );

    // Diagnostic: print the per-repo state so failures are
    // diagnosable. The federation e2e sees node_count > 0 here
    // because `repo.index()` populates the per-repo DB.
    let info = tools_call_text(
        &host,
        "get_repo_info",
        serde_json::json!({"repo_id": "repo"}),
    );
    eprintln!("[get_call_sites] get_repo_info(repo): {info}");

    let search = tools_call_text(
        &host,
        "search_org",
        serde_json::json!({"query": "target", "limit": 10}),
    );
    eprintln!("[get_call_sites] search_org(target): {search}");

    // Use the envelope helper (not the panic-on-error text helper) so
    // we can probe multiple inputs in one go. The name-resolution
    // path is currently broken (it returns "Node not found" even
    // though the per-repo DB has the node — separate bug, see
    // `find_node_by_name` regression in `GraphDatabase`); the
    // id-resolution path works, so we use the node id to drive the
    // assertion. A future fix to the name path can add a name-based
    // variant of this test.
    use common::tools_call_envelope;
    let env_by_name = tools_call_envelope(
        &host,
        "get_call_sites",
        serde_json::json!({"symbol": "target", "repo_id": "repo"}),
    );
    eprintln!("[get_call_sites] by name: {env_by_name}");
    let env_by_id = tools_call_envelope(
        &host,
        "get_call_sites",
        serde_json::json!({"symbol": "d4037d74-1985-56a8-ae27-9bcba45f638c", "repo_id": "repo"}),
    );
    eprintln!("[get_call_sites] by id: {env_by_id}");

    let text = env_by_id
        .pointer("/result/content/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    // 1. The response uses the multi-call heading (6 calls in 1
    //    caller), not the single-call variant. Pin the format so a
    //    regression that reverts to "1 found" (the format the bug
    //    shipped with) is caught.
    assert!(
        text.contains("(6 call(s) across 1 function(s))"),
        "get_call_sites must report the multi-call heading; got:\n{text}"
    );

    // 2. The response lists each distinct call line. The caller
    //    spans lines 2..9; the six calls are on lines 3..8. A
    //    regression to "1 site, 2-9" (the bug) would fail this.
    assert!(
        text.contains("lines 3, 4, 5, 6, 7, 8"),
        "get_call_sites must report each of the six distinct call \
         lines (3, 4, 5, 6, 7, 8) rather than one enclosing-function \
         range; got:\n{text}"
    );
}
