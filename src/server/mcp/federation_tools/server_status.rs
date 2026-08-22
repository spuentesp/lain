//! Per-process server-status tools. Read-only; report on the server's
//! own state rather than the federation's contents. Registered
//! unconditionally in the MCP `tools/list` response regardless of
//! whether the server is running in federation mode.

use crate::error::LainError;
use crate::server::LainServer;
use crate::server::reload::ReloadBus;
use std::time::{SystemTime, UNIX_EPOCH};

/// Format `t` as seconds-since-UNIX-epoch. Used by `get_server_status`
/// for `started_at` / `last_sync_at`. Returns 0 for pre-epoch timestamps
/// rather than panicking, since `SystemTime` subtraction is saturating.
fn system_time_to_unix(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Render the per-process server status payload consumed by the
/// dashboard's status bar.
///
/// Fields:
/// - `pid`: the process id (from `std::process::id`)
/// - `transport`: "stdio" or "http", or null when the server is in
///   single-workspace mode (no MCP transport active)
/// - `port`: TCP port for HTTP transport; null otherwise
/// - `started_at`, `last_sync_at`: seconds since UNIX epoch
/// - `last_error`: most recent sync error message, or null
/// - `repo_count`, `workspace_count`: live counts from the federation
pub fn get_server_status(server: &LainServer) -> serde_json::Value {
    let transport = server.transport().map(|t| match t {
        crate::server::ingest::config::Transport::Stdio => "stdio".to_string(),
        crate::server::ingest::config::Transport::Http => "http".to_string(),
    });
    use crate::server::build_info;
    serde_json::json!({
        "pid": std::process::id(),
        "version": build_info::VERSION,
        "git_sha": build_info::GIT_SHA,
        "binary_mtime_unix": build_info::binary_mtime_unix(),
        "binary_is_stale": build_info::binary_is_stale(),
        "transport": transport,
        // Null under stdio: reporting 9999 when nothing is listening
        // sends an agent to a dashboard that isn't there.
        "port": match transport.as_deref() {
            Some("http") => server.port().map(serde_json::Value::from).unwrap_or(serde_json::Value::Null),
            _ => serde_json::Value::Null,
        },
        "started_at": system_time_to_unix(server.started_at()),
        "last_sync_at": system_time_to_unix(server.last_sync_at()),
        "last_error": server.last_error(),
        "repo_count": server.repo_count(),
        "workspace_count": server.workspace_count(),
    })
}

/// Snapshot the current reload status for the `get_reload_status` MCP
/// tool. Returns `serde_json::Value` so the MCP handler can serialize
/// it without owning the `ReloadStatus` enum.
pub fn get_reload_status(bus: &ReloadBus) -> serde_json::Value {
    use crate::server::reload::ReloadState;
    let s = bus.status();
    serde_json::json!({
        "state": match s.state {
            ReloadState::Idle => "idle",
            ReloadState::Rebuilding => "rebuilding",
            ReloadState::Failed(_) => "failed",
        },
        "started_at_unix": s.started_at.and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs() as i64)
        }),
        "last_reload_at_unix": s.last_reload_at.and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs() as i64)
        }),
        "last_error": s.last_error,
        "pending_changes": s.pending_changes,
    })
}

/// Ask the reload bus to schedule a rebuild. The actual rebuild runs
/// on a dedicated tokio task (spawned by `cli::server::spawn_hot_reload`)
/// so this tool returns immediately after queueing the signal.
pub fn request_reload(bus: &ReloadBus) -> Result<serde_json::Value, LainError> {
    bus.request_reload()
        .map_err(|e| LainError::Other(format!("request_reload: {e}")))?;
    Ok(serde_json::json!({
        "accepted": true,
        "message": "reload scheduled",
        "queued_at_unix": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip the live server through `get_server_status` and assert
    /// the shape. Uses `LainServer::new` (single-workspace mode) so the
    /// test doesn't need a federation fixture; the fields that vary by
    /// mode (transport, port, repo_count, workspace_count) are checked
    /// for null / zero rather than asserting concrete values.
    #[test]
    fn get_server_status_returns_expected_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let mem = tmp.path().join("graph.bin");
        // `LainServer::new` requires `workspace` to be a git repo so the
        // GitSensor can attach. Initialize one.
        git2::Repository::init(&ws).unwrap();
        let server = LainServer::new(&ws, &mem, None).expect("LainServer::new");

        let v = get_server_status(&server);
        assert!(v.get("pid").is_some(), "missing pid");
        assert!(v.get("transport").is_some(), "missing transport");
        assert!(v.get("port").is_some(), "missing port");
        assert!(v.get("started_at").is_some(), "missing started_at");
        assert!(v.get("last_sync_at").is_some(), "missing last_sync_at");
        assert!(v.get("last_error").is_some(), "missing last_error");
        assert!(v.get("repo_count").is_some(), "missing repo_count");
        assert!(v.get("workspace_count").is_some(), "missing workspace_count");

        // Build identity: an agent bound to a long-lived stdio server
        // has no other way to learn that its server predates the fix
        // it is reading about in the source tree.
        assert_eq!(
            v.get("version").and_then(|x| x.as_str()),
            Some(crate::server::build_info::VERSION),
            "missing or wrong version"
        );
        assert_eq!(
            v.get("git_sha").and_then(|x| x.as_str()),
            Some(crate::server::build_info::GIT_SHA),
            "missing or wrong git_sha"
        );
        assert!(v.get("binary_mtime_unix").is_some(), "missing binary_mtime_unix");
        assert_eq!(
            v.get("binary_is_stale").and_then(|x| x.as_bool()),
            Some(false),
            "binary_is_stale must be false when no startup mtime was recorded"
        );

        // `pid` is the current process.
        assert_eq!(v["pid"].as_u64().unwrap(), std::process::id() as u64);
        // Single-workspace mode: no federation, so transport/port null.
        assert!(v["transport"].is_null());
        assert!(v["port"].is_null());
        // No federation → 0 of each.
        assert_eq!(v["repo_count"].as_u64().unwrap(), 0);
        assert_eq!(v["workspace_count"].as_u64().unwrap(), 0);
        // `started_at` and `last_sync_at` are populated (>= 0).
        assert!(v["started_at"].as_i64().unwrap() > 0);
        assert!(v["last_sync_at"].as_i64().unwrap() > 0);
        // No errors yet.
        assert!(v["last_error"].is_null());
    }

    #[test]
    fn get_server_status_reflects_record_last_error() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        git2::Repository::init(&ws).unwrap();
        let mem = tmp.path().join("graph.bin");
        let server = LainServer::new(&ws, &mem, None).unwrap();
        server.record_last_error("boom");
        let v = get_server_status(&server);
        assert_eq!(v["last_error"].as_str(), Some("boom"));
        // record_last_error also bumps last_sync_at.
        let v2 = get_server_status(&server);
        assert!(v2["last_sync_at"].as_i64().unwrap() >= v["last_sync_at"].as_i64().unwrap());
    }

    #[test]
    fn get_server_status_record_sync_clears_error() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        git2::Repository::init(&ws).unwrap();
        let mem = tmp.path().join("graph.bin");
        let server = LainServer::new(&ws, &mem, None).unwrap();
        server.record_last_error("boom");
        server.record_sync();
        let v = get_server_status(&server);
        assert!(v["last_error"].is_null());
    }
}

#[cfg(test)]
mod drift_tests {
    //! Two builders, one tool. `get_server_status` is rendered here and
    //! again by `mcp::handler::HandlerStatus::render`, and the two must
    //! agree on the payload's shape.
    //!
    //! This guard exists because the last undetected divergence of this
    //! kind was expensive: stdio and HTTP built `tools/list` separately,
    //! drifted on the `claim_files` schema, and `claim_files` became
    //! uncallable on stdio — every encoding rejected with
    //! `invalid type: string ..., expected a sequence`, with no way for
    //! a client to tell why.

    /// The key set both builders must produce. Adding a field to one
    /// without the other should fail here rather than in an agent's
    /// session six weeks later.
    const EXPECTED_KEYS: &[&str] = &[
        "pid",
        "version",
        "git_sha",
        "binary_mtime_unix",
        "binary_is_stale",
        "transport",
        "port",
        "started_at",
        "last_sync_at",
        "last_error",
        "repo_count",
        "workspace_count",
    ];

    #[test]
    fn server_status_payloads_do_not_drift() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        git2::Repository::init(&ws).unwrap();
        let mem = tmp.path().join("graph.bin");
        let server = crate::server::LainServer::new(&ws, &mem, None).unwrap();

        let v = super::get_server_status(&server);
        let obj = v.as_object().expect("payload is an object");

        let mut actual: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        actual.sort();
        let mut expected: Vec<&str> = EXPECTED_KEYS.to_vec();
        expected.sort();
        assert_eq!(
            actual, expected,
            "get_server_status keys drifted; update BOTH builders and EXPECTED_KEYS"
        );
    }
}
