//! MCP tool for reading the audit log.
//!
//! Exposes the per-server `audit.jsonl` (Task 2.1) to agents via the
//! `get_audit_log` MCP tool (Task 2.5, PR 2). Agents call this to
//! diff a `plan_revision` against the writes that have actually
//! landed on disk — the durable counterpart to the volatile SSE
//! `edit_landed` stream (`presence_tools.rs`).
//!
//! Args:
//!   - `since_unix: Option<f64>` — drop events whose `ts_unix` is
//!     strictly less than this. `None` returns everything in the
//!     rotation window.
//!   - `path_glob: Option<String>` — keep only events whose `path`
//!     matches the glob. `None` keeps everything.
//!
//! Dispatch: registered in `src/server/mcp/handler.rs` next to the
//! other presence-bearing tools. Mirrors the `run_xxx(server, args)`
//! convention used by `presence_tools` so the handler does not need
//! to know whether the call needs the orchestrator.

use crate::server::audit::{read_audit_log, AuditEvent};
use crate::server::glob_match;
use crate::server::ingest::LainServer;
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct GetAuditLogArgs {
    pub since_unix: Option<f64>,
    pub path_glob: Option<String>,
}

/// Read the audit log from the server's configured state directory
/// (resolved via `LainServer::state_dir_for_audit`), apply the
/// `since_unix` and `path_glob` filters, and return the surviving
/// events as a JSON array. I/O errors and malformed args surface
/// as `Err(String)` so the dispatcher can wrap them in an
/// `is_error: true` `CallToolResult`.
pub fn run_get_audit_log(server: &LainServer, args: Value) -> Result<Value, String> {
    let state_dir = server.state_dir_for_audit();
    run_get_audit_log_with_dir(&state_dir, args)
}

/// Pure form of [`run_get_audit_log`] used by the test suite: takes
/// the audit directory explicitly instead of going through the
/// orchestrator. The public runner is a one-line wrapper that
/// resolves the directory via `LainServer::state_dir_for_audit()`,
/// keeping the tool logic itself decoupled from the server
/// construction so a unit test can pre-populate a `tempdir` without
/// touching `~/.local/lain/state`.
pub fn run_get_audit_log_with_dir(state_dir: &Path, args: Value) -> Result<Value, String> {
    let a: GetAuditLogArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let mut events = read_audit_log(state_dir, a.since_unix).map_err(|e| e.to_string())?;
    if let Some(pattern) = a.path_glob {
        events.retain(|e| glob_match::simple(&pattern, &e.path));
    }
    serde_json::to_value(events).map_err(|e| e.to_string())
}

/// Convenience: the raw filtered events, before JSON serialization.
/// Used by the `get_audit_log_filters_by_path_glob` test so the
/// assertions can inspect `AuditEvent` fields directly.
#[cfg(test)]
pub(crate) fn read_filtered(
    state_dir: &Path,
    since_unix: Option<f64>,
    path_glob: Option<&str>,
) -> Vec<AuditEvent> {
    let mut events = read_audit_log(state_dir, since_unix).expect("read_audit_log");
    if let Some(pattern) = path_glob {
        events.retain(|e| glob_match::simple(pattern, &e.path));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::audit::{append_edit_event, AuditEvent};
    use crate::server::presence::AgentId;
    use std::path::PathBuf;

    fn sample(ts: f64, agent: &str, path: &str) -> AuditEvent {
        AuditEvent {
            ts_unix: ts,
            agent_id: AgentId(agent.to_string()),
            path: PathBuf::from(path),
            claim_set: vec![],
            racers: vec![],
            plan_revision: None,
            landed_revision: 1,
        }
    }

    /// Pre-populate `audit.jsonl` with two events at distinct paths,
    /// filter with `path_glob = "/b/**"`, and assert exactly the
    /// matching event survives. This is the smoke test for the
    /// whole tool: it exercises the dispatcher arg parser, the
    /// `read_audit_log` IO path, the glob filter, and the JSON
    /// serialization round-trip in one shot.
    #[test]
    fn get_audit_log_filters_by_path_glob() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();

        append_edit_event(state_dir, &sample(1.0, "a-1", "/a.rs")).unwrap();
        append_edit_event(state_dir, &sample(2.0, "a-2", "/b/foo.rs")).unwrap();

        let filtered = read_filtered(state_dir, None, Some("/b/**"));
        assert_eq!(
            filtered.len(),
            1,
            "expected only /b/foo.rs to survive the glob; got {} events",
            filtered.len()
        );
        assert_eq!(filtered[0].path, PathBuf::from("/b/foo.rs"));

        // And the public surface — `run_get_audit_log_with_dir` —
        // returns a JSON array carrying both fields. Confirms the
        // dispatcher's `serde_json::to_value` step is wired.
        let value = run_get_audit_log_with_dir(
            state_dir,
            serde_json::json!({ "since_unix": null, "path_glob": "/b/**" }),
        )
        .expect("run_get_audit_log_with_dir");
        let arr = value.as_array().expect("top-level value must be a JSON array");
        assert_eq!(arr.len(), 1, "JSON array should hold one event, got {arr:?}");
        assert_eq!(arr[0]["path"], serde_json::json!("/b/foo.rs"));
    }

    /// `since_unix` cuts on `ts_unix` independently of `path_glob`,
    /// and the two filters compose. Confirms the brief's "filters
    /// out events older than `since_unix`" clause round-trips
    /// through the dispatcher.
    #[test]
    fn since_unix_and_path_glob_compose() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();

        append_edit_event(state_dir, &sample(100.0, "old", "/b/old.rs")).unwrap();
        append_edit_event(state_dir, &sample(200.0, "new", "/b/new.rs")).unwrap();

        let filtered = read_filtered(state_dir, Some(150.0), Some("/b/**"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].agent_id.0, "new");
        assert_eq!(filtered[0].path, PathBuf::from("/b/new.rs"));
    }
}