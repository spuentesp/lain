mod common;

use common::{
    boot_single_repo_in_dir, git_init_committed, tools_call_envelope, wait_for_repo_index,
};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

struct RawSnapshotFixture {
    _tmp: tempfile::TempDir,
    root: std::path::PathBuf,
    host: String,
    _server: Option<common::ServerGuard>,
    repos_yaml: std::path::PathBuf,
    snapshot: String,
}

impl RawSnapshotFixture {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().expect("fixture tempdir");
        let root = tmp.path().to_path_buf();
        let repo = root.join("orders");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(
            repo.join("src/lib.rs"),
            "pub fn old_callee() -> u32 { 42 }\n\npub fn old_caller() -> u32 {\n    old_callee()\n}\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"orders\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        git_init_committed(&repo);
        let commit = git(&repo, &["rev-parse", "HEAD"]);
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
        let (host, server) =
            boot_single_repo_in_dir(&root, &repo, &repos_yaml, &["old_callee", "old_caller"]);
        let prepared = tools_call_envelope(
            &host,
            "prepare_snapshot",
            json!({"repos": {"orders": commit}, "wait_ms": 60_000}),
        );
        assert_eq!(
            prepared.pointer("/result/isError").and_then(Value::as_bool),
            Some(false),
            "prepare_snapshot failed: {prepared}"
        );
        let prepared_text = prepared
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .expect("snapshot text");
        let prepared_json: Value = serde_json::from_str(prepared_text).expect("snapshot json");
        assert_eq!(prepared_json["data"]["state"], "ready");
        let snapshot = prepared_json["data"]["snapshot"]
            .as_str()
            .expect("snapshot id")
            .to_string();

        std::fs::write(
            repo.join("src/lib.rs"),
            "pub fn live_only_caller() -> u32 { 99 }\n",
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(
            &repo,
            &[
                "-c",
                "user.email=test@lain",
                "-c",
                "user.name=test",
                "commit",
                "-q",
                "-m",
                "live replacement",
            ],
        );
        let sync = tools_call_envelope(&host, "sync_state", json!({}));
        assert_eq!(
            sync.pointer("/result/isError").and_then(Value::as_bool),
            Some(false),
            "sync_state failed: {sync}"
        );
        wait_for_repo_index(&host, &["live_only_caller"]);

        Self {
            _tmp: tmp,
            root: root.clone(),
            host,
            _server: Some(server),
            repos_yaml,
            snapshot,
        }
    }

    fn call(&self, name: &str, args: Value) -> Value {
        let mut args = args;
        args["snapshot"] = json!(self.snapshot);
        if args.get("repo_id").is_none() {
            args["repo_id"] = json!("orders");
        }
        tools_call_envelope(&self.host, name, args)
    }

    /// Stop the server and wait for it to exit, so it releases the
    /// handles it holds on the mirror. Windows refuses to rename a
    /// directory that any process still has open.
    fn stop_server(&mut self) {
        if let Some(mut server) = self._server.take() {
            let _ = server.child.kill();
            let _ = server.child.wait();
        }
    }

    /// Boot a fresh server against the same config, on a new port.
    /// Used after `stop_server` when the test needs the server back.
    async fn restart_server(&mut self) {
        let port = common::free_port();
        let host = format!("127.0.0.1:{port}");
        let server = common::boot_server(port, &self.repos_yaml);
        common::wait_for_health(&host, std::time::Duration::from_secs(60));
        self.host = host;
        self._server = Some(server);
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git command");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn text(result: &Value) -> &str {
    result
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

#[tokio::test]
async fn all_raw_snapshot_reads_stay_on_the_pinned_graph_and_source() {
    let fixture = RawSnapshotFixture::new().await;
    let calls = [
        (
            "query_graph",
            json!({"spec": {"ops": [{"op": "find", "name": "old_callee"}]}}),
        ),
        ("find_symbol", json!({"name": "old_callee"})),
        ("get_context", json!({"symbol": "old_callee"})),
        ("get_call_sites", json!({"symbol": "old_callee"})),
        (
            "get_cross_runtime_callers",
            json!({"node_id": "old_callee"}),
        ),
        ("explain_symbol", json!({"symbol": "old_callee"})),
        ("assess_change", json!({"symbol": "old_callee"})),
        ("explain_dispatch", json!({"symbol": "old_callee"})),
    ];
    for (name, args) in calls {
        let result = fixture.call(name, args);
        assert_eq!(
            result.pointer("/result/isError").and_then(Value::as_bool),
            Some(false),
            "{name} failed: {result}"
        );
        let body = text(&result);
        assert!(
            body.contains("old_callee"),
            "{name} lost pinned symbol: {body}"
        );
        assert!(
            !body.contains("live_only_caller"),
            "{name} read live state: {body}"
        );
    }

    let context = fixture.call("get_context", json!({"symbol": "old_callee"}));
    assert!(
        text(&context).contains("42"),
        "pinned source body missing: {context}"
    );
    let sites = fixture.call("get_call_sites", json!({"symbol": "old_callee"}));
    assert!(
        text(&sites).contains("old_caller"),
        "pinned caller missing: {sites}"
    );
}

#[tokio::test]
async fn unknown_raw_snapshot_is_an_error_not_a_live_fallback() {
    let fixture = RawSnapshotFixture::new().await;
    let result = tools_call_envelope(
        &fixture.host,
        "find_symbol",
        json!({"name": "live_only_caller", "snapshot": "snap_does_not_exist", "repo_id": "orders"}),
    );
    assert_eq!(
        result.pointer("/result/isError").and_then(Value::as_bool),
        Some(true),
        "unknown snapshot silently answered from the live graph: {result}"
    );
    assert!(
        text(&result).contains("snapshot"),
        "error should name the snapshot: {result}"
    );
    assert_eq!(
        result.pointer("/result/structuredContent/error/code"),
        Some(&Value::String("snapshot_not_found".into())),
        "unknown snapshot must have a stable structured error: {result}"
    );
}

#[tokio::test]
async fn excluded_snapshot_repo_is_an_error_not_a_live_fallback() {
    let fixture = RawSnapshotFixture::new().await;
    let result = fixture.call(
        "find_symbol",
        json!({"name": "live_only_caller", "repo_id": "billing"}),
    );
    assert_eq!(
        result.pointer("/result/isError").and_then(Value::as_bool),
        Some(true),
        "unregistered snapshot repo silently answered live: {result}"
    );
    assert_eq!(
        result.pointer("/result/structuredContent/error/code"),
        Some(&Value::String("repo_not_registered".into())),
        "excluded repo must have a stable structured error: {result}"
    );
}

#[tokio::test]
async fn non_string_snapshot_is_an_error_not_a_live_fallback() {
    let fixture = RawSnapshotFixture::new().await;
    let result = tools_call_envelope(
        &fixture.host,
        "find_symbol",
        json!({"name": "live_only_caller", "snapshot": 123, "repo_id": "orders"}),
    );
    assert_eq!(
        result.pointer("/result/isError").and_then(Value::as_bool),
        Some(true),
        "non-string snapshot silently answered from live data: {result}"
    );
    assert!(text(&result).contains("snapshot"), "{result}");
    assert_eq!(
        result.pointer("/result/structuredContent/error/code"),
        Some(&Value::String("invalid_argument".into())),
        "invalid snapshot argument must have a stable structured error: {result}"
    );
}

#[tokio::test]
async fn arbitrary_snapshot_values_never_fall_back_to_live_data() {
    let fixture = RawSnapshotFixture::new().await;
    for value in [
        Value::Null,
        Value::Bool(true),
        Value::Bool(false),
        json!(123),
        json!(123.5),
        json!(["snap_test"]),
        json!({"id": "snap_test"}),
        json!("snap_does_not_exist"),
    ] {
        let result = tools_call_envelope(
            &fixture.host,
            "find_symbol",
            json!({"name": "live_only_caller", "snapshot": value, "repo_id": "orders"}),
        );
        assert_eq!(
            result.pointer("/result/isError").and_then(Value::as_bool),
            Some(true),
            "snapshot value unexpectedly answered from live data: {result}"
        );
    }
}

#[tokio::test]
async fn unreadable_snapshot_source_is_an_error_in_structured_and_legacy_tools() {
    let mut fixture = RawSnapshotFixture::new().await;
    let mirror = fixture.root.join("data/mirrors/orders.git");
    let parked = fixture.root.join("data/mirrors/orders.git.parked");
    // The running server holds the mirror's worktree registration open.
    // Windows refuses to rename a directory with an open handle, so the
    // server is stopped first; the mirror is then unreachable and the
    // snapshot reads must report the failure rather than fall back to
    // live data.
    fixture.stop_server();
    std::fs::rename(&mirror, &parked).unwrap();
    fixture.restart_server().await;

    for (tool, args) in [
        ("get_context", json!({"symbol": "old_callee"})),
        ("explain_symbol", json!({"symbol": "old_callee"})),
        ("get_call_sites", json!({"symbol": "old_callee"})),
    ] {
        let result = fixture.call(tool, args);
        assert_eq!(
            result.pointer("/result/isError").and_then(Value::as_bool),
            Some(true),
            "{tool} hid the snapshot source failure: {result}"
        );
    }
}
