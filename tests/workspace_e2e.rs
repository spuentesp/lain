//! Workspace-aware federation tests.
//!
//! Each test that needs the federation runtime builds a 3-crate fixture
//! (shared, auth-svc, db-client) in tempdirs, writes repos.yaml +
//! workspaces.yaml, and loads via `load_federation_with_workspace` — the
//! explicit per-repo indexing pattern (NOT `load_federation` which
//! doesn't call repo.index()).
//!
//! Tests 1 and 3 are pure validation (no federation runtime needed). Tests
//! 2, 4, 5 require the federation runtime; they hang in environments
//! where `rust-analyzer` isn't available (the `RepoIndex::index().await`
//! call blocks on LSP hydration with no timeout). Run in a working env to
//! verify the federation-dependent tests; the pure-validation tests pass
//! in any env.
//!
//! Gated behind `--features test-utils` like the existing benchmark file.

use lain::federation::loader::load_federation_with_workspace;
use lain::federation::workspace::{WorkspacesFile, WorkspaceSpec};
use lain::mcp::federation_tools::get_active_workspace;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Initialize a tempdir as a git repo with a local identity and one
/// initial commit so `GitSensor::new` + `repo.index()` work. Mirrors
/// `init_temp_git_repo` from `tests/federation_integration.rs:20`.
fn init_git_repo_with_commit(dir: &Path) {
    let status = Command::new("git")
        .arg("init").arg("--quiet").arg("--initial-branch=main").arg(dir)
        .status()
        .expect("git init failed to start");
    assert!(status.success(), "git init failed: {status}");

    let run = |args: &[&str]| {
        let status = Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .expect("git failed to start");
        assert!(status.success(), "git {args:?} failed: {status}");
    };
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Workspace Test"]);
    run(&["add", "-A"]);
    run(&["commit", "--quiet", "-m", "initial"]);
}

/// Build a 3-crate fixture: shared (defines `hash`), db-client (defines
/// `verify_token`), auth-svc (calls into shared's `hash`). `init_git_repo`
/// runs on each subdir.
fn write_three_dependent_crates(root: &Path) {
    let shared = root.join("shared");
    let db_client = root.join("db-client");
    let auth_svc = root.join("auth-svc");

    let shared_lib = "\
pub fn verify_token(s: &str) -> bool { !s.is_empty() }
pub fn hash(s: &str) -> u64 {
    let inner = inner_hash(s);
    inner
}
pub fn inner_hash(s: &str) -> u64 {
    let mut h: u64 = 0;
    for b in s.bytes() { h = h.wrapping_mul(31).wrapping_add(b as u64); }
    h
}
";
    let db_client_lib = "\
pub fn connect() -> bool {
    crate::verify_token(\"...\")
}
pub fn verify_token(s: &str) -> bool { false } // duplicate symbol for AmbiguousSymbol
";
    let auth_svc_lib = "\
pub fn auth(s: &str) -> bool {
    shared::hash(s) > 0
}
";

    for (sub, name, lib) in [
        (&shared, "shared", shared_lib),
        (&db_client, "db-client", db_client_lib),
        (&auth_svc, "auth-svc", auth_svc_lib),
    ] {
        std::fs::create_dir_all(sub.join("src")).unwrap();
        let mut cargo = format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
        if name != "shared" {
            cargo.push_str("[dependencies]\nshared = { path = \"../shared\" }\n");
        }
        std::fs::write(sub.join("Cargo.toml"), cargo).unwrap();
        std::fs::write(sub.join("src/lib.rs"), lib).unwrap();
        init_git_repo_with_commit(sub);
    }
}

fn write_repos_yaml(root: &Path, repo_ids: &[&str]) -> PathBuf {
    let cfg = root.join("repos.yaml");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let mut yaml = format!("data_dir: {}\nrepos:\n", data.display());
    for id in repo_ids {
        yaml.push_str(&format!(
            "  - id: {id}\n    source: {{ type: workspace_dir, path: {} }}\n",
            root.join(id).display()
        ));
    }
    std::fs::write(&cfg, yaml).unwrap();
    cfg
}

fn write_workspaces_yaml(root: &Path, ws_name: &str, members: &[&str]) -> PathBuf {
    let path = root.join("workspaces.yaml");
    let mut yaml = String::from("workspaces:\n");
    yaml.push_str(&format!(
        "  - name: {ws_name}\n    members: [{}]\n",
        members.iter().map(|m| format!("\"{m}\"")).collect::<Vec<_>>().join(", ")
    ));
    std::fs::write(&path, yaml).unwrap();
    path
}

#[test]
fn workspace_config_loads_and_validates_members() {
    let f = WorkspacesFile {
        default: None,
        workspaces: vec![WorkspaceSpec {
            name: "backend-team".into(),
            description: None,
            source: None,
            members: vec!["auth-svc".into(), "billing-svc".into()],
        }],
    };
    f.validate().expect("valid workspace should pass");
}

#[test]
fn workspace_rejects_sub_two_repos() {
    let f = WorkspacesFile {
        default: None,
        workspaces: vec![WorkspaceSpec {
            name: "tiny".into(),
            description: None,
            source: None,
            members: vec!["only".into()],
        }],
    };
    assert!(f.validate().is_err(), "workspace with 1 member must fail validation");
}

#[tokio::test]
async fn workspace_rejects_unknown_repo_id() {
    let tmp = tempfile::tempdir().unwrap();
    write_three_dependent_crates(tmp.path());
    // repos.yaml only has shared + db-client; workspace references billing-svc.
    let repos_yaml = write_repos_yaml(tmp.path(), &["shared", "db-client"]);
    write_workspaces_yaml(tmp.path(), "backend-team", &["shared", "billing-svc"]);
    let result = load_federation_with_workspace(&repos_yaml, tmp.path().join("workspaces.yaml").as_path(), "backend-team").await;
    let err = match result {
        Ok(_) => panic!("billing-svc not in repos.yaml; load should have errored"),
        Err(e) => e,
    };
    let msg = format!("{err:?}");
    assert!(
        msg.contains("billing-svc"),
        "expected error to mention missing repo id 'billing-svc', got: {msg}"
    );
}

#[tokio::test]
async fn workspace_filters_repos_to_members() {
    let tmp = tempfile::tempdir().unwrap();
    // 5 repos total; workspace has 3.
    for i in 0..5 {
        let sub = tmp.path().join(format!("r{i}"));
        std::fs::create_dir_all(sub.join("src")).unwrap();
        std::fs::write(
            sub.join("Cargo.toml"),
            format!("[package]\nname = \"r{i}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        ).unwrap();
        std::fs::write(sub.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        init_git_repo_with_commit(&sub);
    }
    let mut repo_ids: Vec<String> = (0..5).map(|i| format!("r{i}")).collect();
    let repos_yaml = write_repos_yaml(tmp.path(), &{
        let s: Vec<&str> = repo_ids.iter().map(String::as_str).collect();
        s
    });
    write_workspaces_yaml(tmp.path(), "three", &["r0", "r1", "r2"]);
    let fed = load_federation_with_workspace(&repos_yaml, tmp.path().join("workspaces.yaml").as_path(), "three").await.unwrap();
    let loaded: Vec<String> = fed.list_repos().into_iter().map(|(id, _)| id.to_string()).collect();
    assert_eq!(loaded.len(), 3, "expected 3 loaded repos, got {loaded:?}");
    for keep in &["r0", "r1", "r2"] {
        repo_ids.retain(|r| r != keep);
    }
    for drop in &repo_ids {
        assert!(!loaded.contains(drop), "repo {drop} should be filtered out");
    }
}

#[tokio::test]
async fn workspace_mcp_get_active_workspace_returns_correct_subset() {
    let tmp = tempfile::tempdir().unwrap();
    write_three_dependent_crates(tmp.path());
    let repos_yaml = write_repos_yaml(tmp.path(), &["shared", "db-client"]);
    write_workspaces_yaml(tmp.path(), "auth-ws", &["shared", "db-client"]);
    let fed = load_federation_with_workspace(&repos_yaml, tmp.path().join("workspaces.yaml").as_path(), "auth-ws").await.unwrap();
    let workspaces = WorkspacesFile::load(tmp.path().join("workspaces.yaml").as_path()).unwrap();
    let info = get_active_workspace(&fed, &workspaces).expect("active workspace should resolve");
    assert_eq!(info.name, "auth-ws");
    assert_eq!(info.members, vec!["shared", "db-client"]);
}