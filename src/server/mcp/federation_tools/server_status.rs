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
    serde_json::json!({
        "pid": std::process::id(),
        "transport": transport,
        "port": server.port(),
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
