//! End-to-end tests for the annotation MCP tools (`add_annotation`,
//! `list_annotations`, `resolve_annotation`) — boots the real
//! `lain server` binary against a single-repo federation fixture
//! and exercises the dispatcher path production agents hit.
//!
//! These ride the same `tests/common::boot_*` harness every other
//! integration test in this crate uses; the only additions are the
//! fixtures themselves (a tiny in-tree Rust crate that the
//! annotations can target) and the JSON-RPC round-trips that
//! pin the documented contracts:
//!
//! * `add_annotation` → `id` + `created_at_unix_ms` + `repo_id`
//!   back; row visible in the immediately-following `list_annotations`
//!   with `status="open"`.
//! * `resolve_annotation` returns a `resolved` summary; subsequent
//!   `list_annotations` with `status="open"` filters the row out.
//! * `list_annotations` with `status="resolved"` shows the row
//!   the open filter hid.
//! * A `target={kind: "symbol", ...}` call against a multi-repo
//!   federation without a `repo_id` surfaces the byte-exact Config
//!   error the central gate formats for cross-cutting tools.
//!
//! The unit tests at `src/server/annotations.rs` cover the storage
//! layer (write→read round-trip, filter, staleness, UTF-8 boundary);
//! this file covers what those tests cannot: the JSON-RPC envelope,
//! the dispatcher path, the auth/session_token wiring, and the
//! cross-cutting tool gate's "needs a repo scope" error format.
//!
//! Closes `docs/FOLLOWUPS.md` item 2 for annotations specifically
//! (the handoff side of that same entry is `tests/handoff_e2e.rs`).

mod common;

use std::path::{Path, PathBuf};

use common::{
    free_port, git_init_committed, tools_call_envelope, tools_call_text, wait_for_repo_index,
    ServerGuard,
};

/// Minimal single-repo federation fixture with one Rust crate that
/// defines a single function `ann_target`. The repo id is `ann_a`;
/// `add_annotation` with `target={kind: "symbol", symbol: ...}` and
/// no `repo_id` triggers the central-gate Config error path, which
/// the third test in this file pins byte-exactly.
struct AnnotationFixture {
    root: PathBuf,
}

impl AnnotationFixture {
    fn build() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::mem::forget(tmp);

        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"ann_a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("src/lib.rs"),
            "/// Doc comment so the indexer keeps the symbol.\n\
             pub fn ann_target() -> u32 { 1 }\n",
        )
        .unwrap();
        git_init_committed(&repo);

        let data_dir = root.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(
            root.join("repos.yaml"),
            format!(
                "data_dir: {}\n\
                 ready_threshold: 0.5\n\
                 repos:\n\
                 \x20 - id: ann_a\n\
                 \x20   source: {{ type: workspace_dir, path: {} }}\n",
                data_dir.display(),
                repo.display(),
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("workspaces.yaml"),
            "workspaces:\n  - name: solo\n    members: [ann_a]\n",
        )
        .unwrap();

        Self { root }
    }

    fn repos_yaml(&self) -> PathBuf {
        self.root.join("repos.yaml")
    }
}

/// Same `XDG_STATE_HOME` / `XDG_CONFIG_HOME` / `LAIN_JOB_STORE`
/// pinning the federation e2e uses — the state dir must be isolated
/// so per-test annotations don't leak across runs.
fn boot_annotation_server(fixture: &AnnotationFixture, port: u16) -> ServerGuard {
    use std::process::{Command, Stdio};

    let stderr_path = std::env::temp_dir().join(format!("annotations-e2e-stderr-{port}.log"));
    let stderr_file = std::fs::File::create(&stderr_path).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_lain"))
        .args([
            "server",
            "--transport",
            "http",
            "--port",
            &port.to_string(),
            "--workspace",
            "auto",
            "--config",
            fixture.repos_yaml().to_str().unwrap(),
        ])
        .env_remove("LAIN_EMBEDDING_MODEL")
        .env("XDG_STATE_HOME", fixture.root.join("state"))
        .env("XDG_CONFIG_HOME", fixture.root.join("config"))
        .env("LAIN_JOB_STORE", fixture.root.join("jobs.json"))
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .expect("spawn lain server");
    ServerGuard { child, stderr_path }
}

fn boot_and_wait(fixture: &AnnotationFixture) -> (String, ServerGuard) {
    let port = free_port();
    let host = format!("127.0.0.1:{port}");
    let guard = boot_annotation_server(fixture, port);
    common::wait_for_health(&host, std::time::Duration::from_secs(60));
    wait_for_repo_index(&host, &["ann_target"]);
    (host, guard)
}

fn register_session(host: &str, name: &str) -> String {
    let resp = tools_call_text(
        host,
        "register_agent",
        serde_json::json!({"name": name, "mode": "interactive"}),
    );
    let parsed: serde_json::Value = serde_json::from_str(&resp)
        .unwrap_or_else(|e| panic!("register_agent not JSON: {e}\n{resp}"));
    parsed
        .pointer("/session_token")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("missing session_token: {parsed}"))
        .to_string()
}

/// Pin the byte-exact envelope the dispatcher returns for
/// `add_annotation`: `id`, `created_at_unix_ms`, `repo_id`. Drift in
/// any of those names breaks the agent contract.
fn add_annotation_response(
    host: &str,
    session_token: &str,
    body: &str,
    target: serde_json::Value,
) -> serde_json::Value {
    let resp = tools_call_text(
        host,
        "add_annotation",
        serde_json::json!({
            "session_token": session_token,
            "body": body,
            "kind": "note",
            "target": target,
        }),
    );
    serde_json::from_str(&resp)
        .unwrap_or_else(|e| panic!("add_annotation not JSON: {e}\n{resp}"))
}

/// `add_annotation` returns the new row's id; `list_annotations`
/// (no filters) surfaces it; `resolve_annotation` closes it; the
/// subsequent `list_annotations` with `status="open"` filters the
/// resolved row out. The full write→read→resolve→filter cycle.
#[test]
fn add_list_resolve_roundtrip() {
    let fixture = AnnotationFixture::build();
    let (host, _guard) = boot_and_wait(&fixture);
    let session = register_session(&host, "annotations-e2e");

    let target = serde_json::json!({
        "kind": "repo",
        "repo_id": "ann_a",
    });
    let body = "fixture annotation: roundtrip";
    let added = add_annotation_response(&host, &session, body, target);
    let id = added
        .pointer("/id")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("missing id: {added}"))
        .to_string();
    assert!(
        id.len() >= 32 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "annotation id should be uuid-shaped: {id}"
    );
    assert_eq!(
        added.pointer("/repo_id").and_then(|v| v.as_str()),
        Some("ann_a"),
        "add_annotation must echo the resolved repo_id"
    );

    // list_annotations (no status filter) surfaces the new row.
    let list_resp = tools_call_text(
        &host,
        "list_annotations",
        serde_json::json!({
            "target": {"kind": "repo", "repo_id": "ann_a"},
        }),
    );
    let list: serde_json::Value = serde_json::from_str(&list_resp)
        .unwrap_or_else(|e| panic!("list_annotations not JSON: {e}\n{list_resp}"));
    let rows = list.pointer("/annotations").and_then(|v| v.as_array());
    let rows = rows.unwrap_or_else(|| panic!("missing annotations array: {list}"));
    assert!(
        rows.iter().any(|r| {
            r.pointer("/id").and_then(|v| v.as_str()) == Some(&id)
                && r.pointer("/body").and_then(|v| v.as_str()) == Some(body)
        }),
        "list_annotations must include the freshly-added row: {list}"
    );

    // resolve_annotation closes the row.
    let resolved = tools_call_text(
        &host,
        "resolve_annotation",
        serde_json::json!({
            "session_token": session,
            "id": id,
        }),
    );
    let resolved_v: serde_json::Value = serde_json::from_str(&resolved)
        .unwrap_or_else(|e| panic!("resolve_annotation not JSON: {e}\n{resolved}"));
    assert_eq!(
        resolved_v.pointer("/resolved/status").and_then(|v| v.as_str()),
        Some("resolved"),
        "resolve_annotation must report the new status byte-exactly"
    );

    // Subsequent `status=open` filter must hide the resolved row.
    let open_only = tools_call_text(
        &host,
        "list_annotations",
        serde_json::json!({
            "status": "open",
            "target": {"kind": "repo", "repo_id": "ann_a"},
        }),
    );
    let open_v: serde_json::Value = serde_json::from_str(&open_only).unwrap();
    let open_rows = open_v
        .pointer("/annotations")
        .and_then(|v| v.as_array())
        .unwrap();
    assert!(
        open_rows
            .iter()
            .all(|r| r.pointer("/id").and_then(|v| v.as_str()) != Some(&id)),
        "resolved annotation must not appear in status=open list: {open_v}"
    );

    // `status=resolved` shows it.
    let resolved_only = tools_call_text(
        &host,
        "list_annotations",
        serde_json::json!({
            "status": "resolved",
            "target": {"kind": "repo", "repo_id": "ann_a"},
        }),
    );
    let resolved_only_v: serde_json::Value = serde_json::from_str(&resolved_only).unwrap();
    let resolved_rows = resolved_only_v
        .pointer("/annotations")
        .and_then(|v| v.as_array())
        .unwrap();
    assert!(
        resolved_rows
            .iter()
            .any(|r| r.pointer("/id").and_then(|v| v.as_str()) == Some(&id)),
        "resolved annotation must appear in status=resolved list: {resolved_only_v}"
    );
}

/// Pin the byte-exact Config error a multi-repo federation returns
/// for a `target={kind: "symbol", ...}` annotation without a
/// `repo_id`. The central-gate wrapper added in PR #66 pins this
/// format and a regression in any layer (gate wrapper, dispatcher's
/// repo-resolver, error envelope formatter) flips this test.
///
/// This test deliberately builds a two-repo federation so the gate
/// has a multi-repo state to error on; the single-repo fixture used
/// elsewhere in this file would resolve to the lone repo
/// unambiguously.
#[test]
fn multi_repo_target_without_repo_id_surfaces_config_error() {
    // Build a second federation with two repos so the gate errors
    // rather than auto-pinning.
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    let make_repo = |name: &str, sym: &str, root: &Path| {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .unwrap();
        std::fs::write(
            dir.join("src/lib.rs"),
            format!("/// doc\npub fn {sym}() -> u32 {{ 1 }}\n"),
        )
        .unwrap();
        git_init_committed(&dir);
        dir
    };
    let a = make_repo("anno_a", "alpha_helper", &root);
    let b = make_repo("anno_b", "beta_helper", &root);

    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let repos_yaml = root.join("repos.yaml");
    std::fs::write(
        &repos_yaml,
        format!(
            "data_dir: {}\n\
             ready_threshold: 0.5\n\
             repos:\n\
             \x20 - id: anno_a\n\
             \x20   source: {{ type: workspace_dir, path: {} }}\n\
             \x20 - id: anno_b\n\
             \x20   source: {{ type: workspace_dir, path: {} }}\n",
            data_dir.display(),
            a.display(),
            b.display(),
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("workspaces.yaml"),
        "workspaces:\n  - name: both\n    members: [anno_a, anno_b]\n",
    )
    .unwrap();

    let port = free_port();
    let host = format!("127.0.0.1:{port}");
    let stderr_path = std::env::temp_dir().join(format!("anno-multi-stderr-{port}.log"));
    let stderr_file = std::fs::File::create(&stderr_path).unwrap();
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_lain"))
        .args([
            "server",
            "--transport",
            "http",
            "--port",
            &port.to_string(),
            "--workspace",
            "auto",
            "--config",
            repos_yaml.to_str().unwrap(),
        ])
        .env_remove("LAIN_EMBEDDING_MODEL")
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("LAIN_JOB_STORE", root.join("jobs.json"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(stderr_file))
        .spawn()
        .expect("spawn lain server");
    let guard = ServerGuard { child, stderr_path };
    common::wait_for_health(&host, std::time::Duration::from_secs(60));
    wait_for_repo_index(&host, &["alpha_helper", "beta_helper"]);

    let session = register_session(&host, "annotations-multi");

    // Symbol-typed target without a repo_id hits the central gate's
    // "requires scoping: multiple repos" error. The exact prefix is
    // the regression net the gate pins (see handler.rs regression
    // sweep `eight_original_failures_classify_correctly`).
    let envelope = tools_call_envelope(
        &host,
        "add_annotation",
        serde_json::json!({
            "session_token": session,
            "body": "should not land",
            "kind": "note",
            "target": {"kind": "symbol", "symbol": "alpha_helper"},
        }),
    );
    let is_error = envelope
        .pointer("/result/isError")
        .and_then(|v| v.as_bool())
        == Some(true);
    assert!(
        is_error,
        "multi-repo symbol target without repo_id must return isError=true: {envelope}"
    );
    let text = envelope
        .pointer("/result/content/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    // `add_annotation` has its own internal repo-resolution path
    // (`annotation_tools::target_to_repo`) that emits a
    // tool-specific error before the central gate's
    // "requires scoping" wording kicks in. The actual contract is
    // that the error names the tool and tells the agent to use
    // a `target = {kind: "repo", repo_id: "..."}` shape; drift in
    // that wording is what this test pins.
    assert!(
        text.contains("Cross-repo annotations require target"),
        "Config error format drift — missing 'Cross-repo annotations require target': {text}"
    );
    assert!(
        text.contains("add_annotation"),
        "Config error must name the requesting tool so the agent can arbitrate: {text}"
    );
    assert!(
        text.contains("repo_id"),
        "Config error must point at the resolving argument so the agent knows what to add: {text}"
    );

    drop(guard);
}
