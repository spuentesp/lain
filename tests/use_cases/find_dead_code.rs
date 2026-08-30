//! End-to-end proving test for `find_dead_code`.
//!
//! Boots a real `lain server --transport http` against a small Rust
//! fixture, calls the `find_dead_code` MCP tool, and asserts on the
//! Markdown response.
//!
//! The fixture has six functions in one file:
//!   - `orchestrate()` (entry point — doc-comment mentions its name
//!     so the textual reference filter excludes it; without this
//!     the tool flags every entry point as dead)
//!   - `helper_a()`, `helper_b()` (live, called by `orchestrate`)
//!   - `dead_one()`, `dead_two()` (truly dead: no callers, no callees)
//!   - `#[test] fn test_helper()` (no callers by design, but the
//!     `#[test]` attribute must exclude it from the dead set)
//!
//! Assertions:
//!   1. `dead_one` and `dead_two` ARE in the response.
//!   2. `orchestrate` is NOT in the response (excluded by name-reference).
//!   3. `helper_a` and `helper_b` are NOT in the response (have callers).
//!   4. `test_helper` is NOT in the response (excluded by `#[test]` attribute).
//!
//! The last assertion is the one the wishlist audit found broken
//! historically — the previous behavior reported `#[test]` functions
//! as dead. If the attribute detection regresses, this test fires.
//!
//! Requires `rust-analyzer` on PATH so the fixture's `#[test]`
//! attribute is detected. Tree-sitter also reads `#[test]` directly,
//! so the test attribute should be present even without LSP, but
//! the live MCP round-trip still requires a working federation boot.

#[path = "../common/mod.rs"]
mod common;
use common::{boot_single_repo, git_init_committed, tools_call_text};

#[test]
fn find_dead_code_reports_dead_and_excludes_tests_and_live() {
    // Bail loudly if rust-analyzer is missing — the fixture's
    // `#[test]` attribute needs the indexing path to be discovered.
    which::which("rust-analyzer").expect("rust-analyzer must be on PATH");

    let project = tempfile::tempdir().unwrap();
    let repo_dir = project.path().join("repo");
    std::fs::create_dir_all(repo_dir.join("src")).unwrap();
    std::fs::write(
        repo_dir.join("Cargo.toml"),
        "[package]\nname = \"dead-code-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        repo_dir.join("src/lib.rs"),
        "/// The entry point: `orchestrate` calls into the helpers.\n\
         pub fn orchestrate() -> u32 {\n    \
         helper_a() + helper_b()\n\
         }\n\
         pub fn helper_a() -> u32 { 1 }\n\
         pub fn helper_b() -> u32 { 2 }\n\
         /// Dead — nothing calls this.\n\
         pub fn dead_one() -> u32 { 1 }\n\
         /// Dead — nothing calls this either.\n\
         pub fn dead_two() -> u32 { 2 }\n\
         #[test]\n\
         fn test_helper() { assert_eq!(2 + 2, 4); }\n",
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
        &["dead_one", "dead_two", "orchestrate"],
    );

    let text = tools_call_text(&host, "find_dead_code", serde_json::json!({}));

    // 1. The two truly dead functions are reported.
    assert!(
        text.contains("dead_one"),
        "find_dead_code must report `dead_one`; got:\n{text}"
    );
    assert!(
        text.contains("dead_two"),
        "find_dead_code must report `dead_two`; got:\n{text}"
    );

    // 2. Live functions (have callers) are NOT reported. `orchestrate`
    //    is also an entry point but is excluded by the
    //    `name_referenced_anywhere` filter (the doc-comment mentions
    //    the name) — if the textual-sweep filter regresses to its
    //    pre-fix behavior, this assertion fires.
    assert!(
        !text.contains("orchestrate"),
        "find_dead_code must NOT report `orchestrate` (excluded by \
         name-references; the doc-comment mentions it); got:\n{text}"
    );
    assert!(
        !text.contains("helper_a"),
        "find_dead_code must NOT report `helper_a` (has callers); got:\n{text}"
    );
    assert!(
        !text.contains("helper_b"),
        "find_dead_code must NOT report `helper_b` (has callers); got:\n{text}"
    );

    // 3. The `#[test]` function is excluded. This is the regression
    //    the wishlist audit found: previous behavior reported
    //    `#[test]` functions as dead. If the attribute detection
    //    regresses, this assertion fires.
    assert!(
        !text.contains("test_helper"),
        "find_dead_code must NOT report `test_helper` (excluded by \
         `#[test]` attribute); got:\n{text}"
    );
}
