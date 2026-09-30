use crate::federation::loader::{load_federation, load_federation_with_workspace};

#[tokio::test]
async fn loads_minimal_config_with_workspace_dir_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("repos.yaml");
    std::fs::write(
        &cfg_path,
        format!(
            r#"
data_dir: {}
repos:
  - id: ws
    source: {{ type: workspace_dir, path: {} }}
"#,
            tmp.path().join("data").display(),
            tmp.path().join("ws").display()
        ),
    )
    .unwrap();
    // `RepoIndex::new` instantiates a `GitSensor` against the source's local
    // path, so the path must be a real git repo. Initialize a throwaway repo
    // in a fresh tempdir; the test's behavior (load a config, verify the
    // single repo is listed) is unchanged.
    let ws_dir = tmp.path().join("ws");
    std::fs::create_dir_all(&ws_dir).unwrap();
    git2::Repository::init(&ws_dir).unwrap();
    std::fs::create_dir_all(tmp.path().join("data")).unwrap();

    let fed = load_federation(&cfg_path).await.unwrap();
    let listed = fed.list_repos();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0.as_str(), "ws");
}

/// A workspace that `lain workspaces init` just wrote has 0 members until
/// `lain workspaces add` fills it. `WorkspacesFile::validate` accepts that
/// transient state so init can save the file, but the federation load must
/// refuse it: coming up with zero repos is a silently empty federation
/// (vacuous readiness, no error).
#[tokio::test]
async fn load_federation_with_workspace_refuses_zero_member_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("repos.yaml");
    std::fs::write(
        &cfg_path,
        format!(
            "data_dir: {}\nrepos: []\n",
            tmp.path().join("data").display()
        ),
    )
    .unwrap();
    let workspaces_path = tmp.path().join("workspaces.yaml");
    std::fs::write(
        &workspaces_path,
        r#"
workspaces:
  - name: pending
    members: []
    source:
      type: workspace_clone
      url: https://example.com/ws.git
"#,
    )
    .unwrap();

    let err = match load_federation_with_workspace(&cfg_path, &workspaces_path, "pending").await {
        Ok(_) => panic!("0-member workspace must not load into a federation"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("workspace 'pending' has no members yet"),
        "error must name the empty workspace, got: {msg}"
    );
    assert!(
        msg.contains("lain workspaces add pending --repo <repo-id>"),
        "error must name the remedy, got: {msg}"
    );
}
