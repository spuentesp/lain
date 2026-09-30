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

/// `repos.yaml` with `repos: []` must not load into an all-repos
/// federation: zero repos is a silently empty federation (vacuous
/// readiness, no error) — the same failure mode the 0-member workspace
/// refusal below guards, so it gets the same shape of error: name the
/// problem and the remedy.
#[tokio::test]
async fn load_federation_refuses_empty_repos_list() {
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

    let err = match load_federation(&cfg_path).await {
        Ok(_) => panic!("repos.yaml with no repos must not load into a federation"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("repos.yaml declares no repos"),
        "error must name the empty config, got: {msg}"
    );
    assert!(
        msg.contains("lain repos add"),
        "error must name the remedy, got: {msg}"
    );
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

/// Cold start must keep refusing a workspace that names a repo id missing
/// from `repos.yaml`. The hot-reload path tolerates dangling members so a
/// `lain repos remove` converges (see `repos_for_workspace`); starting up
/// against that state is still a config error and must say so.
#[tokio::test]
async fn load_federation_with_workspace_refuses_member_missing_from_repos_yaml() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("repos.yaml");
    std::fs::write(
        &cfg_path,
        format!(
            "data_dir: {}\nrepos:\n  - id: present\n    source: {{ type: workspace_dir, path: {} }}\n",
            tmp.path().join("data").display(),
            tmp.path().join("present").display()
        ),
    )
    .unwrap();
    let workspaces_path = tmp.path().join("workspaces.yaml");
    std::fs::write(
        &workspaces_path,
        "workspaces:\n  - name: team\n    members: [present, ghost]\n",
    )
    .unwrap();

    let err = match load_federation_with_workspace(&cfg_path, &workspaces_path, "team").await {
        Ok(_) => panic!("a workspace member missing from repos.yaml must not load cold"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("ghost"),
        "error must name the dangling repo id, got: {msg}"
    );
    assert!(
        msg.contains("references repos not in repos.yaml"),
        "got: {msg}"
    );
}
