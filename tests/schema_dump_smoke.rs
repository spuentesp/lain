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
