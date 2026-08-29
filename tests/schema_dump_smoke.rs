//! Smoke tests for the `lain schema dump` CLI subcommand. Pinned by
//! defect D-L2: the on-disk schema dump must match the wire
//! `tools/list` payload so the doc and the protocol cannot drift.

use std::process::Command;

fn lain() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lain"))
}

/// `lain schema dump --out <tmp>` writes a JSON file containing every
/// tool that `tools/list` returns when the server runs with federation
/// + workspaces. Pin the surface by name: at minimum the well-known
/// tools from each subset must appear.
#[test]
fn lain_schema_dump_writes_tools_list_shape() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out_path = tmp.path().join("tool-schema.json");

    let out = lain()
        .args(["schema", "dump", "--out", out_path.to_str().unwrap()])
        .output()
        .expect("run lain schema dump");
    assert!(
        out.status.success(),
        "lain schema dump failed (status {:?}):\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(out_path.is_file(), "output file not written: {out_path:?}");

    let raw = std::fs::read_to_string(&out_path).expect("read schema file");
    let parsed: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("schema file is not valid JSON: {e}\n{raw}"));

    // Root must be an array.
    let tools = parsed
        .as_array()
        .unwrap_or_else(|| panic!("root must be a JSON array, got {parsed}"));

    // Every tool must have the wire shape: {name, description, inputSchema}.
    for t in tools {
        assert!(t.get("name").and_then(|n| n.as_str()).is_some(),
                "tool missing `name`: {t}");
        assert!(t.get("description").is_some(),
                "tool missing `description`: {t}");
        assert!(t.get("inputSchema").is_some(),
                "tool missing `inputSchema`: {t}");
    }

    // Spot-check: at least one tool from each subset must appear, so a
    // silent omission of one of the five sources shows up here.
    let names: std::collections::HashSet<&str> =
        tools.iter().filter_map(|t| t.get("name").and_then(|n| n.as_str())).collect();
    // From ToolRegistry (inventory-registered handlers): `query_graph` is
    // a long-stable member of this surface.
    assert!(names.contains("query_graph"),
            "ToolRegistry surface missing `query_graph`: {names:?}");
    // From special_tool_definitions: `get_health`.
    assert!(names.contains("get_health"),
            "special surface missing `get_health`: {names:?}");
    // From FEDERATION_TOOL_DEFS: `list_repos`.
    assert!(names.contains("list_repos"),
            "federation surface missing `list_repos`: {names:?}");
    // From WORKSPACE_TOOL_DEFS: `list_workspaces`.
    assert!(names.contains("list_workspaces"),
            "workspace surface missing `list_workspaces`: {names:?}");
    // From SERVER_TOOL_DEFS: `get_server_status`.
    assert!(names.contains("get_server_status"),
            "server surface missing `get_server_status`: {names:?}");

    // `claim_files` must declare `agent_id`, `session_token`, AND a
    // `files` arg typed as `array` — the wire-shape property that
    // caused the live e2e bug (D-H3). This locks both the surface and
    // the per-arg typing the doc promises.
    let claim = tools.iter()
        .find(|t| t.get("name").and_then(|n| n.as_str()) == Some("claim_files"))
        .expect("claim_files in schema");
    let required = claim.get("inputSchema")
        .and_then(|s| s.get("required"))
        .and_then(|r| r.as_array())
        .expect("claim_files.inputSchema.required must be an array");
    let required: std::collections::HashSet<&str> = required.iter()
        .filter_map(|v| v.as_str()).collect();
    for arg in ["agent_id", "session_token", "files"] {
        assert!(required.contains(arg),
                "claim_files must require `{arg}`, got: {required:?}");
    }
    let files_type = claim.get("inputSchema")
        .and_then(|s| s.get("properties"))
        .and_then(|p| p.get("files"))
        .and_then(|f| f.get("type"))
        .and_then(|t| t.as_str());
    assert_eq!(files_type, Some("array"),
               "claim_files.files must be typed array, got: {claim}");
}

/// Boot a real `lain server --transport http`, send a JSON-RPC
/// `tools/list`, and byte-compare the returned `result.tools` to the
/// on-disk `docs/tool-schema.json`. This is the property D-L2
/// requires: the doc and the protocol cannot drift.
///
/// The server is launched with `--workspace auto` and an empty
/// `repos.yaml` so the live `tools/list` returns the *full* surface
/// (federation tools + workspace tools + server tools + inventory
/// tools + special tools). The CLI subcommand emits the same full
/// surface (it does not conditionalize on federation/workspaces being
/// configured). Both sides apply `inert_tool_names(&embedder)` via
/// the stub embedder, so `semantic_search` is dropped identically on
/// both sides. The byte-comparison then holds.
///
/// Workspace tools require `workspaces.yaml` to exist next to
/// `repos.yaml` so the server picks the `with_federation_and_workspaces_*`
/// constructor (which sets `LainHandler.workspaces = Some(...)`,
/// triggering the `if workspaces.is_some() { tools.extend(...); }`
/// branch in the HTTP `tools/list` arm). The test writes a minimal
/// valid `workspaces.yaml` (one empty workspace) so workspace tools
/// are advertised; the federation is empty (`repos: []`) so no
/// indexing work runs and startup stays under the 15s health-poll
/// timeout.
///
/// `XDG_CONFIG_HOME` is redirected to a tempdir so the
/// `~/.config/lain/active_workspace` pointer file is empty — without
/// this, a developer machine with a stale active workspace pointer
/// would cause `--workspace auto` to resolve to a named workspace
/// instead of falling through to "all repos", and the live
/// `tools/list` would diverge.
#[test]
fn live_tools_list_byte_matches_on_disk_schema_dump() {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };

    // Empty federation (no repos indexed). `workspaces.yaml` exists so
    // the workspace tools are advertised; one empty workspace is
    // enough — `WorkspacesFile::validate` only checks ≥1 member and
    // does not require the member to be in the federation.
    let project = tempfile::tempdir().unwrap();
    let data_dir = project.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(
        project.path().join("repos.yaml"),
        format!("data_dir: {}\nrepos: []\n", data_dir.display()),
    )
    .unwrap();
    std::fs::write(
        project.path().join("workspaces.yaml"),
        "workspaces:\n  - name: drift-detection\n    members: [nonexistent-repo]\n",
    )
    .unwrap();

    let state = tempfile::tempdir().unwrap();
    let xdg_config = tempfile::tempdir().unwrap();
    let mut server = Command::new(env!("CARGO_BIN_EXE_lain"))
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
        // Suppress LAIN_EMBEDDING_MODEL so the server falls back to the
        // relative `models/all-MiniLM-L6-v2.onnx` path, which doesn't
        // exist in the repo root and forces a stub embedder — matching
        // the dump subcommand's stub-embedder semantics so `semantic_search`
        // is dropped on both sides identically.
        .env_remove("LAIN_EMBEDDING_MODEL")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn lain server");

    // Poll /health until ready (same pattern as doctor_smoke).
    let host = format!("127.0.0.1:{port}");
    let start = Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(30) {
            let _ = server.kill();
            let _ = server.wait();
            panic!("server did not become healthy within 30s");
        }
        let attempt: std::io::Result<()> = (|| {
            let mut stream = TcpStream::connect(&host)?;
            stream.write_all(
                format!("GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )?;
            let mut response = String::new();
            stream.read_to_string(&mut response)?;
            if response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200") {
                Ok(())
            } else {
                Err(std::io::Error::other(format!("not 200: {response}")))
            }
        })();
        if attempt.is_ok() { break; }
        std::thread::sleep(Duration::from_millis(100));
    }

    // JSON-RPC tools/list against the live server. The HTTP
    // transport exposes it at POST /mcp with Content-Type:
    // application/json (see handler.rs:1824 and the command-center
    // UI at command_center/app.js:704).
    let rpc_body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {},
    });
    let mut stream = TcpStream::connect(&host).expect("connect /mcp");
    stream.write_all(
        format!(
            "POST /mcp HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\
             Content-Type: application/json\r\nContent-Length: {len}\r\n\r\n{body}",
            host = host,
            len = rpc_body.to_string().len(),
            body = rpc_body,
        )
        .as_bytes(),
    )
    .expect("write rpc");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read rpc response");

    let _ = server.kill();
    let _ = server.wait();

    let body_start = response.find("\r\n\r\n").expect("http body") + 4;
    let body = &response[body_start..];
    let rpc: serde_json::Value = serde_json::from_str(body)
        .unwrap_or_else(|e| panic!("rpc response not JSON: {e}\n{body}"));
    let live_tools = rpc.get("result")
        .and_then(|r| r.get("tools"))
        .cloned()
        .unwrap_or_else(|| panic!("rpc response missing result.tools: {rpc}"));

    // Read the committed docs/tool-schema.json (regenerated by `make schema`).
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let on_disk_path = manifest_dir.join("docs/tool-schema.json");
    assert!(on_disk_path.is_file(),
            "docs/tool-schema.json missing — run `make schema` first: {on_disk_path:?}");
    let on_disk_raw = std::fs::read_to_string(&on_disk_path).expect("read docs/tool-schema.json");
    let on_disk: serde_json::Value =
        serde_json::from_str(&on_disk_raw).expect("parse docs/tool-schema.json");

    // Canonicalize (re-serialize to drop formatting whitespace and
    // normalize map-key ordering) and byte-compare.
    let live_canonical = serde_json::to_string(&live_tools).expect("canon live");
    let on_disk_canonical = serde_json::to_string(&on_disk).expect("canon disk");

    assert_eq!(
        live_canonical, on_disk_canonical,
        "tools/list and docs/tool-schema.json have drifted.\n\
         Re-run `make schema` and commit the result.\n\
         -- live:\n{live_tools}\n-- on-disk:\n{on_disk}",
        live_tools = live_tools,
        on_disk = on_disk,
    );
}
