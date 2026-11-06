mod common;

use common::{boot_single_repo_in_dir, git_init_committed, tools_call_text};
use serde_json::Value;

#[tokio::test]
async fn capabilities_report_relations_emitters_and_snapshot_support() {
    let tmp = tempfile::tempdir().expect("fixture tempdir");
    let root = tmp.path().to_path_buf();
    let repo = root.join("orders");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn ready() {}\n").unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"orders\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    git_init_committed(&repo);
    let repos_yaml = root.join("repos.yaml");
    std::fs::write(
        &repos_yaml,
        format!(
            "data_dir: {}/data\nrepos:\n  - id: orders\n    source:\n      type: workspace_dir\n      path: {}\n",
            root.display(),
            repo.display()
        ),
    )
    .unwrap();
    let (host, _server) = boot_single_repo_in_dir(&root, &repo, &repos_yaml, &["ready"]);
    let raw = tools_call_text(&host, "get_capabilities", serde_json::json!({}));
    let value: Value = serde_json::from_str(&raw).expect("capabilities json");
    let relations = value["repositories"][0]["relations"]
        .as_array()
        .expect("relation availability");
    assert!(relations.len() >= 20);
    assert!(relations.iter().any(|relation| {
        relation["name"] == "Calls" && relation["state"] == "ready" && relation["indexed"] == true
    }));
    let emitters = value["repositories"][0]["emitters"]
        .as_array()
        .expect("emitter readiness");
    assert!(emitters
        .iter()
        .any(|emitter| { emitter["name"] == "django_orm" && emitter["state"] == "ready" }));
    assert_eq!(value["snapshots"]["supported"], true);
    assert_eq!(value["snapshots"]["state"], "ready");
}
