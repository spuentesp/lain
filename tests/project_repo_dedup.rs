use lain::federation::loader::load_federation;
use lain::federation::repo_id::RepoId;

#[tokio::test]
async fn project_repo_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("repos.yaml");
    std::fs::write(&cfg, "data_dir: ./.lain/data\nrepos:\n- id: probe\n  source: { type: workspace_dir, path: /home/sebastian/lain }\n").unwrap();
    let fed = load_federation(&cfg).await.unwrap();
    let id = RepoId::new("probe").unwrap();
    // First projection
    fed.project_repo(&id).await.unwrap();
    let first = fed.backend().edge_count();
    // Second projection — idempotent?
    fed.project_repo(&id).await.unwrap();
    let second = fed.backend().edge_count();
    println!("first={} second={}", first, second);
    assert_eq!(first, second, "second projection must not add edges");
}
