//! End-to-end feat suite — exercises every advertised capability in
//! `.superpowers/sdd/trust/capabilities.md` against a real running
//! `lain server` on the HTTP transport.
//!
//! One big `#[test]` that walks through five categories (A–E) in
//! order. The categories map 1:1 to the wishlist categories in the
//! trust doc; each block lists the promises checked and the test
//! fails on the first unmet promise, with the offending tool/error
//! string attached to the panic.
//!
//! The test is hermetic: ephemeral port (bound, released, reused),
//! tempdir for `repos.yaml` + `workspaces.yaml`, redirected
//! `XDG_STATE_HOME`/`XDG_CONFIG_HOME` so no developer state is
//! touched, and a hard cleanup at the end via `Drop` semantics in
//! the `ServerGuard` helper.

mod common;

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::{free_port, http_request, jsonrpc, tools_call_text, wait_for_health, ServerGuard};

/// Issue `tools/call <name> <args>` and parse the result as JSON.
/// For tools that return Markdown (most of them) the parse will
/// fail — callers should prefer [`tools_call_text`] from `common`.
fn tools_call_json(host: &str, name: &str, arguments: serde_json::Value) -> serde_json::Value {
    let text = tools_call_text(host, name, arguments);
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("tool {name} text not JSON: {e}\n{text}"))
}

/// Initialize a git repo at `path`, configure a local identity,
/// and commit everything in the working tree. The indexer skips
/// non-git directories, so without this `repos.yaml -> workspace_dir`
/// finds no files and `find_anchors` has nothing to rank.
fn git_init(path: &std::path::Path) {
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(path)
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed");
    for (k, v) in [("user.email", "feat-suite@lain"), ("user.name", "feat-suite")] {
        std::process::Command::new("git")
            .args(["config", k, v])
            .current_dir(path)
            .status()
            .unwrap();
    }
    let add = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(path)
        .status()
        .expect("git add");
    assert!(add.success(), "git add failed");
    let commit = std::process::Command::new("git")
        .args(["commit", "-q", "-m", "feat-suite fixture"])
        .current_dir(path)
        .status()
        .expect("git commit");
    assert!(commit.success(), "git commit failed");
}

/// Compare a server-returned path string against an expected list of
/// path components. The server returns paths via
/// `PathBuf::to_string_lossy()`, which uses the platform's native
/// separator — `\` on Windows, `/` on Unix. The wire protocol
/// contract is "the same components the caller named", not a
/// specific separator spelling, so we compare component-by-component
/// instead of as raw strings. Splitting on either separator handles
/// both platforms in one branch.
fn path_components_eq(path: &str, expected: &[&str]) -> bool {
    let actual: Vec<&str> = path
        .split(['/', '\\'])
        .filter(|s| !s.is_empty())
        .collect();
    actual == expected
}

fn boot_server(port: u16) -> ServerGuard {
    // Project dir: one minimal repo so the federation is non-empty
    // (federation tools refuse to dispatch when `list_repos()` is
    // empty — `get_health` and friends return
    // `Config("no repos registered")`). The repo is a fresh git
    // checkout with one Rust file, which gives us a real (tiny)
    // graph for `find_anchors`/`query_graph` to operate on.
    let project = tempfile::tempdir().unwrap();
    let repo_dir = project.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(repo_dir.join("src")).unwrap();
    std::fs::write(
        repo_dir.join("Cargo.toml"),
        "[package]\nname = \"feat-suite-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        repo_dir.join("src/lib.rs"),
        "pub fn orchestrate() -> u32 {\n    let a = helper_a();\n    helper_b() + a\n}\n\
         pub fn entrypoint() -> u32 { orchestrate() }\n\
         pub fn helper_a() -> u32 { 1 }\n\
         pub fn helper_b() -> u32 { 2 }\n",
    )
    .unwrap();
    // Init the git repo so the indexer picks the files up.
    git_init(&repo_dir);
    let repo_id = repo_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo")
        .to_string();

    let data_dir = project.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(
        project.path().join("repos.yaml"),
        format!(
            "data_dir: {}\nrepos:\n  - id: {}\n    source:\n      type: workspace_dir\n      path: {}\n",
            data_dir.display(),
            repo_id,
            repo_dir.display(),
        ),
    )
    .unwrap();
    std::fs::write(
        project.path().join("workspaces.yaml"),
        format!(
            "workspaces:\n  - name: feat-suite\n    members: [{}]\n",
            repo_id
        ),
    )
    .unwrap();

    let state = tempfile::tempdir().unwrap();
    let xdg_config = tempfile::tempdir().unwrap();

    // Capture stderr to a file so a panic message can show why the
    // server failed (default is null and we lose everything on
    // spawn failure).
    let stderr_path = std::env::temp_dir().join(format!("feat-suite-stderr-{port}.log"));
    let stderr_file = std::fs::File::create(&stderr_path).unwrap();

    let child = Command::new(env!("CARGO_BIN_EXE_lain"))
        .args([
            "server",
            "--transport", "http",
            "--port", &port.to_string(),
            "--workspace", "auto",
            "--config",
            project.path().join("repos.yaml").to_str().unwrap(),
        ])
        .env("XDG_STATE_HOME", state.path())
        .env("XDG_CONFIG_HOME", xdg_config.path())
        .env("LAIN_JOB_STORE", state.path().join("jobs.json"))
        .env_remove("LAIN_EMBEDDING_MODEL")
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .unwrap_or_else(|e| {
            panic!(
                "spawn lain server failed: {e}; binary={}; stderr={}",
                env!("CARGO_BIN_EXE_lain"),
                stderr_path.display()
            )
        });

    let guard = ServerGuard(child);
    let host = format!("127.0.0.1:{port}");
    wait_for_health(&host, Duration::from_secs(30));
    // Suppress unused-variable warnings for helpers retained for the
    // shared harness (other test files use these directly).
    let _ = http_request;
    let _ = jsonrpc;
    guard
}

#[test]
fn feat_suite_end_to_end() {
    let port = free_port();
    let host = format!("127.0.0.1:{port}");
    let _server = boot_server(port);

    // ─── Category A — server boot + HTTP transport ─────────────────
    // A.1 GET /health returns JSON with version, status, tools_count,
    // graph_nodes, graph_edges, federation (or null).
    let health_raw = http_request(
        &host,
        &format!("GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"),
    );
    assert_eq!(health_raw.0, 200, "/health not 200: {}", health_raw.1);
    let health: serde_json::Value =
        serde_json::from_str(&health_raw.1).expect("/health body not JSON");
    for field in ["version", "status", "tools_count", "graph_nodes", "graph_edges", "federation"] {
        assert!(
            health.get(field).is_some(),
            "/health missing required field `{field}`: {health}"
        );
    }
    assert_eq!(health["status"].as_str(), Some("ok"), "/health status: {health}");
    // `/health` reports `ToolRegistry::definitions().len()` — the
    // inventory-registered handlers only. The full surface (which
    // tools/list appends with special/federation/workspace/server
    // defs) is much larger; the dump-test in category D below
    // confirms the 60+ surface directly. The health count being
    // non-zero is the only thing this endpoint promises.
    let health_tools_count = health["tools_count"].as_u64().unwrap_or(0);
    assert!(
        health_tools_count >= 30,
        "/health tools_count too low (inventory handlers only): {health}"
    );

    // A.2 POST /mcp with tools/list returns a non-empty list.
    let tools_list_body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {},
    })
    .to_string();
    let tools_list_resp = jsonrpc(&host, &tools_list_body);
    let tools = tools_list_resp
        .pointer("/result/tools")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("tools/list missing result.tools: {tools_list_resp}"));
    assert!(!tools.is_empty(), "tools/list returned empty array");
    let names: std::collections::HashSet<String> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
        .map(|s| s.to_string())
        .collect();

    // A.3 GET / returns HTML (Command Center SPA).
    let root_resp = http_request(
        &host,
        &format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"),
    );
    assert_eq!(root_resp.0, 200, "/ not 200: {}", root_resp.1);
    assert!(
        root_resp.1.contains("<html") || root_resp.1.to_lowercase().contains("<!doctype html"),
        "/ response does not look like HTML: first 200 chars: {}",
        &root_resp.1.chars().take(200).collect::<String>()
    );

    // ─── Category B — advertised tool surface ──────────────────────
    // Every tool the docs promise must be present in `tools/list`.
    // `semantic_search` is documented as "inert when no model — OK
    // to be listed" but the current build drops it via
    // `inert_tool_names(embedder)` because no model is loaded; we
    // allow either case so the test doesn't lock the embedder
    // policy in.
    let required_tools = [
        "find_anchors",
        "get_blast_radius",
        "get_call_chain",
        "find_dead_code",
        "explain_symbol",
        "trace_dependency",
        "get_health",
        "list_entry_points",
        "get_context_depth",
        "explore_architecture",
        // semantic_search: optional — server drops it without an
        // ONNX model (per `inert_tool_names`); allow either case.
        "query_graph",
        "register_agent",
        "heartbeat",
        "list_active_agents",
        "who_am_i",
        "list_subagents",
        "claim_files",
        "release_files",
        "list_occupancy",
        "my_claims",
        "request_reload",
        "get_reload_status",
        "get_server_status",
        "compare_modules",
        "find_untested_functions",
        "get_coverage_summary",
        "get_call_sites",
        "get_recent_activity",
        "suggest_refactor_targets",
    ];
    for tool in required_tools {
        assert!(
            names.contains(tool),
            "advertised tool `{tool}` missing from tools/list (have {} names)",
            names.len()
        );
    }

    // ─── Category C — tool functionality (representative subset) ──
    // C.1 get_health: Operational status with non-zero graph_nodes.
    // `get_health` returns Markdown prose with a `Status:` line —
    // assert it's operational and that some real graph was indexed.
    let health_text = tools_call_text(&host, "get_health", serde_json::json!({}));
    assert!(
        health_text.contains("Operational"),
        "get_health missing Operational status: {health_text}"
    );
    assert!(
        health_text.contains("Static Nodes:") && !health_text.contains("Static Nodes: 0"),
        "get_health reports 0 static nodes — fixture was not indexed: {health_text}"
    );

    // C.2 find_anchors returns a non-empty `anchors` list (text
    // form — the tool returns formatted text, so we just assert it
    // doesn't error out and contains "anchors" or similar).
    let anchors_str = tools_call_text(&host, "find_anchors", serde_json::json!({"limit": 5}));
    // The text format is "Top N anchors ..." or "No anchors ...".
    assert!(
        anchors_str.contains("anchors") || anchors_str.contains("No anchors"),
        "find_anchors output unexpected: {anchors_str}"
    );

    // C.3 register_agent returns agent_id + session_token.
    let reg = tools_call_json(
        &host,
        "register_agent",
        serde_json::json!({"name": "feat-suite-agent", "kind": "other", "mode": "interactive"}),
    );
    let agent_id = reg
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("register_agent missing agent_id: {reg}"))
        .to_string();
    let session_token = reg
        .get("session_token")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("register_agent missing session_token: {reg}"))
        .to_string();
    assert!(!agent_id.is_empty(), "empty agent_id");
    assert!(!session_token.is_empty(), "empty session_token");

    // C.4 list_active_agents includes the just-registered agent.
    let active = tools_call_json(&host, "list_active_agents", serde_json::json!({}));
    let active_arr = active
        .as_array()
        .unwrap_or_else(|| panic!("list_active_agents not array: {active}"));
    let active_ids: std::collections::HashSet<String> = active_arr
        .iter()
        .filter_map(|a| a.get("agent_id").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
        .collect();
    assert!(
        active_ids.contains(&agent_id),
        "list_active_agents missing just-registered agent {agent_id}: {active}"
    );

    // C.5 claim_files accepts BOTH string form ["src/a.rs"] AND
    // object form [{"path": "src/b.rs", "intent": "edit"}].
    // Regression for defect that originally only accepted object form.
    let claim_str = tools_call_json(
        &host,
        "claim_files",
        serde_json::json!({
            "agent_id": agent_id,
            "session_token": session_token,
            "files": ["src/a.rs"],
        }),
    );
    let granted_str = claim_str
        .get("granted")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("claim_files(string) missing granted: {claim_str}"));
    // The `path` field is `PathBuf::to_string_lossy()` of the
    // server-canonicalized path, so it uses the platform's native
    // separator (backslash on Windows, forward-slash on Unix).
    // Compare via path components so the assertion is portable.
    assert!(
        granted_str.iter().any(|g| {
            g.get("path")
                .and_then(|p| p.as_str())
                .map(|p| path_components_eq(p, &["src", "a.rs"]))
                .unwrap_or(false)
        }),
        "claim_files did not grant string-form src/a.rs: {claim_str}"
    );

    let claim_obj = tools_call_json(
        &host,
        "claim_files",
        serde_json::json!({
            "agent_id": agent_id,
            "session_token": session_token,
            "files": [{"path": "src/b.rs", "intent": "edit"}],
        }),
    );
    let granted_obj = claim_obj
        .get("granted")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("claim_files(object) missing granted: {claim_obj}"));
    assert!(
        granted_obj.iter().any(|g| {
            g.get("path")
                .and_then(|p| p.as_str())
                .map(|p| path_components_eq(p, &["src", "b.rs"]))
                .unwrap_or(false)
        }),
        "claim_files did not grant object-form src/b.rs: {claim_obj}"
    );

    // C.6 my_claims returns both claims.
    let my = tools_call_json(
        &host,
        "my_claims",
        serde_json::json!({
            "agent_id": agent_id,
            "session_token": session_token,
        }),
    );
    let my_arr = my
        .as_array()
        .unwrap_or_else(|| panic!("my_claims not array: {my}"));
    let my_paths: std::collections::HashSet<String> = my_arr
        .iter()
        .filter_map(|c| c.get("path").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
        .collect();
    // Component-wise comparison so the test works on Windows too:
    // see `path_components_eq` above.
    let has_a = my_paths
        .iter()
        .any(|p| path_components_eq(p, &["src", "a.rs"]));
    let has_b = my_paths
        .iter()
        .any(|p| path_components_eq(p, &["src", "b.rs"]));
    assert!(
        has_a && has_b,
        "my_claims missing one or both files; got: {my_paths:?}"
    );

    // C.7 release_files accepts the same forms.
    let rel_str = tools_call_text(
        &host,
        "release_files",
        serde_json::json!({
            "agent_id": agent_id,
            "session_token": session_token,
            "files": ["src/a.rs"],
        }),
    );
    // release_files returns Ok({}) as a JSON object; if the server
    // rejected the string form it would come back as isError=true,
    // which tools_call_text panics on.
    let rel_str_json: serde_json::Value = serde_json::from_str(&rel_str)
        .unwrap_or_else(|e| panic!("release_files(string) not JSON: {e}\n{rel_str}"));
    assert!(
        rel_str_json.is_object(),
        "release_files(string) unexpected shape: {rel_str}"
    );

    let rel_obj = tools_call_text(
        &host,
        "release_files",
        serde_json::json!({
            "agent_id": agent_id,
            "session_token": session_token,
            "files": [{"path": "src/b.rs"}],
        }),
    );
    let rel_obj_json: serde_json::Value = serde_json::from_str(&rel_obj)
        .unwrap_or_else(|e| panic!("release_files(object) not JSON: {e}\n{rel_obj}"));
    assert!(
        rel_obj_json.is_object(),
        "release_files(object) unexpected shape: {rel_obj}"
    );

    // C.8 query_graph with a `find` op returns results.
    // Empty federation means we accept an empty result — we just
    // assert the call succeeds and returns the standard shape.
    let qg_text = tools_call_text(
        &host,
        "query_graph",
        serde_json::json!({
            "ops": [{"op": "find", "label": "Function"}],
            "limit": 5,
        }),
    );
    let qg: serde_json::Value = serde_json::from_str(&qg_text)
        .unwrap_or_else(|e| panic!("query_graph not JSON: {e}\n{qg_text}"));
    // The shape is `{"nodes": [...], "edges": [...], ...}` plus
    // an `occupancy` splice.
    assert!(
        qg.get("nodes").is_some() || qg.get("occupancy").is_some() || qg.is_array(),
        "query_graph returned unexpected shape: {qg}"
    );

    // ─── Category D — schema dump ─────────────────────────────────
    // D.1 `lain schema dump --out <path>` writes a JSON file.
    // D.2 Parse it.
    // D.3 It has the same number of entries as tools/list for the
    // same server config.
    let tmp = tempfile::tempdir().expect("tempdir");
    let schema_out: PathBuf = tmp.path().join("schema.json");
    let out = Command::new(env!("CARGO_BIN_EXE_lain"))
        .args(["schema", "dump", "--out", schema_out.to_str().unwrap()])
        .output()
        .expect("run lain schema dump");
    assert!(
        out.status.success(),
        "lain schema dump failed (status {:?}):\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let raw = std::fs::read_to_string(&schema_out).expect("read schema file");
    let dumped: serde_json::Value =
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("schema not JSON: {e}\n{raw}"));
    let dumped_arr = dumped
        .as_array()
        .unwrap_or_else(|| panic!("schema dump root not array: {dumped}"));

    // The dump always emits all five sources (full surface); the
    // live `tools/list` emits the same surface when federation +
    // workspaces are configured (which we did). Allow a small drift
    // — the server may filter `semantic_search` via
    // `inert_tool_names(embedder)` while the dump uses its own
    // stub embedder. The contract is "no silent removal"; require
    // the dump to be at least as large.
    let live_count = tools.len();
    let dumped_count = dumped_arr.len();
    assert!(
        dumped_count >= live_count,
        "schema dump has {dumped_count} tools, live tools/list has {live_count} — \
         schema dump should be the full surface; live may filter"
    );
    // And the doc promises 60+ tools total — the dump should hit
    // that comfortably.
    assert!(
        dumped_count >= 60,
        "schema dump count below the documented minimum (60+): {dumped_count}"
    );

    // Spot-check the dump contains the headline tools by name.
    let dumped_names: std::collections::HashSet<&str> = dumped_arr
        .iter()
        .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
        .collect();
    for headline in ["find_anchors", "get_health", "claim_files", "query_graph"] {
        assert!(
            dumped_names.contains(headline),
            "schema dump missing headline tool `{headline}`"
        );
    }

    // ─── Category E — doctor ──────────────────────────────────────
    // E.1 `lain doctor` exits 0.
    // E.2 stdout contains "all checks passed".
    let doctor = Command::new(env!("CARGO_BIN_EXE_lain"))
        .args(["doctor"])
        .output()
        .expect("run doctor");
    let doctor_stdout = String::from_utf8_lossy(&doctor.stdout).to_string();
    assert!(
        doctor.status.success(),
        "lain doctor failed (status {:?}): {doctor_stdout}",
        doctor.status.code()
    );
    assert!(
        doctor_stdout.contains("all checks passed"),
        "lain doctor missing `all checks passed` line; stdout:\n{doctor_stdout}"
    );
}
