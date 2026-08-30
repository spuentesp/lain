//! End-to-end proving test for `get_code_snippet` path handling.
//!
//! The wishlist audit notes a path-handling bug; the tool was
//! resolving `src/lib.rs` (workspace-relative) and `/abs/.../src/lib.rs`
//! (absolute) inconsistently, and the `path` resolver in
//! `std::path::Path` does the wrong thing when the path has
//! symlinks or `..` segments. The proving test pins three things:
//!
//!   1. Workspace-relative path (`src/lib.rs`) returns the file.
//!   2. Absolute path (`<repo>/src/lib.rs`) returns the file.
//!   3. Nonexistent path returns an `isError` result that names
//!      the missing path.
//!
//! All three go through the same MCP tool, against a real `lain
//! server` boot, so the path-handling is exercised end-to-end.

#[path = "../common/mod.rs"]
mod common;
use common::{boot_single_repo, git_init_committed, tools_call_envelope, tools_call_text};

#[test]
fn get_code_snippet_resolves_relative_and_absolute_paths_and_rejects_missing() {
    let project = tempfile::tempdir().unwrap();
    let repo_dir = project.path().join("repo");
    std::fs::create_dir_all(repo_dir.join("src")).unwrap();
    std::fs::write(
        repo_dir.join("Cargo.toml"),
        "[package]\nname = \"snippet-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    // A unique marker so the test can assert the response contains
    // *this file's* content and not a same-named file from
    // elsewhere (e.g. the process CWD).
    std::fs::write(
        repo_dir.join("src/lib.rs"),
        "/// SNIPPET_MARKER_42\n\
         pub fn from_fixture() -> u32 { 42 }\n",
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

    let (host, _guard) = boot_single_repo(&repo_dir, &repos_yaml_path, &["from_fixture"]);

    // 1. Workspace-relative path resolves. We pass `repo_id` so the
    //    per-repo tool binds to the per-repo graph and the path is
    //    resolved against the repo's `local_path` rather than the
    //    process CWD (the bug).
    let text = tools_call_text(
        &host,
        "get_code_snippet",
        serde_json::json!({
            "path": "src/lib.rs",
            "repo_id": "repo",
        }),
    );
    assert!(
        text.contains("SNIPPET_MARKER_42"),
        "get_code_snippet(workspace-relative path) must return the \
         fixture file's content; got:\n{text}"
    );

    // 2. Absolute path (constructed from the same repo_dir the
    //    server is reading from) also resolves.
    let abs_path = repo_dir.join("src").join("lib.rs");
    let text = tools_call_text(
        &host,
        "get_code_snippet",
        serde_json::json!({
            "path": abs_path.to_string_lossy(),
            "repo_id": "repo",
        }),
    );
    assert!(
        text.contains("SNIPPET_MARKER_42"),
        "get_code_snippet(absolute path) must return the fixture file's \
         content; got:\n{text}"
    );

    // 3. Nonexistent path returns an error envelope, not a panic.
    let env = tools_call_envelope(
        &host,
        "get_code_snippet",
        serde_json::json!({
            "path": "src/does_not_exist.rs",
            "repo_id": "repo",
        }),
    );
    let is_err = env
        .pointer("/result/isError")
        .and_then(|v| v.as_bool())
        == Some(true);
    assert!(
        is_err,
        "get_code_snippet(nonexistent path) must set isError=true; got: {env}"
    );
    // The error must name the missing path so the agent can
    // correlate the failure with the input it sent. (Wishlist #18.)
    let text = env
        .pointer("/result/content/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        text.contains("does_not_exist.rs"),
        "get_code_snippet error must name the missing path; got: {text}"
    );
}
