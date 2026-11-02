mod common;

use common::{boot_single_repo_in_dir, git_init_committed, tools_call_envelope};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git command");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn text(result: &Value) -> &str {
    result
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

#[tokio::test]
async fn snapshot_symbol_and_source_reads_stay_in_the_requested_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for (repo, value) in [("orders", 42), ("billing", 43)] {
        let path = root.join(repo);
        std::fs::create_dir_all(path.join("src")).unwrap();
        std::fs::write(
            path.join("src/lib.rs"),
            format!("pub fn shared() -> u32 {{\n    {value}\n}}\n"),
        )
        .unwrap();
        std::fs::write(
            path.join("Cargo.toml"),
            format!("[package]\nname = \"{repo}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .unwrap();
        git_init_committed(&path);
    }
    let orders = root.join("orders");
    let billing = root.join("billing");
    let repos_yaml = root.join("repos.yaml");
    std::fs::write(
        &repos_yaml,
        format!(
            "data_dir: {}/data\nrepos:\n  - id: orders\n    source:\n      type: workspace_dir\n      path: {}\n  - id: billing\n    source:\n      type: workspace_dir\n      path: {}\n",
            root.display(),
            orders.display(),
            billing.display()
        ),
    )
    .unwrap();
    let (host, _server) = boot_single_repo_in_dir(root, &orders, &repos_yaml, &[]);
    let prepared = tools_call_envelope(
        &host,
        "prepare_snapshot",
        json!({
            "repos": {
                "orders": git(&orders, &["rev-parse", "HEAD"]),
                "billing": git(&billing, &["rev-parse", "HEAD"]),
            },
            "wait_ms": 60_000,
        }),
    );
    assert_eq!(
        prepared.pointer("/result/isError").and_then(Value::as_bool),
        Some(false),
        "{prepared}"
    );
    let prepared_text = prepared
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap();
    let snapshot = serde_json::from_str::<Value>(prepared_text).unwrap()["data"]["snapshot"]
        .as_str()
        .unwrap()
        .to_string();

    let billing_symbol = tools_call_envelope(
        &host,
        "find_symbol",
        json!({"name": "shared", "snapshot": snapshot, "repo_id": "billing"}),
    );
    assert_eq!(
        billing_symbol
            .pointer("/result/isError")
            .and_then(Value::as_bool),
        Some(false),
        "{billing_symbol}"
    );
    assert!(
        text(&billing_symbol).contains("billing:Function"),
        "{billing_symbol}"
    );
    assert!(
        !text(&billing_symbol).contains("orders:Function"),
        "{billing_symbol}"
    );

    let billing_source = tools_call_envelope(
        &host,
        "explain_symbol",
        json!({"symbol": "shared", "snapshot": snapshot, "repo_id": "billing"}),
    );
    assert_eq!(
        billing_source
            .pointer("/result/isError")
            .and_then(Value::as_bool),
        Some(false),
        "{billing_source}"
    );
    assert!(text(&billing_source).contains("43"), "{billing_source}");
    assert!(!text(&billing_source).contains("42"), "{billing_source}");

    let ambiguous = tools_call_envelope(
        &host,
        "find_symbol",
        json!({"name": "shared", "snapshot": snapshot}),
    );
    assert_eq!(
        ambiguous
            .pointer("/result/isError")
            .and_then(Value::as_bool),
        Some(true),
        "{ambiguous}"
    );
    assert!(text(&ambiguous).contains("repo_id"), "{ambiguous}");
}
