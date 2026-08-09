use crate::federation::loader::load_federation;

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
