//! Per-process server-status tools. Read-only; report on the server's
//! own state rather than the federation's contents. Registered
//! unconditionally in the MCP `tools/list` response regardless of
//! whether the server is running in federation mode.

use crate::error::LainError;
use crate::server::LainServer;
use crate::server::reload::ReloadBus;
use std::time::SystemTime;

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
/// Everything `get_server_status` reports that is not derived from the
/// build or the process itself.
///
/// Exists so the payload has exactly one builder. It had two — this
/// module for the single-workspace path and `mcp::handler::HandlerStatus`
/// for HTTP — which drifted: the build-identity fields were added here
/// and the HTTP transport kept serving a payload without them. That is
/// the same failure mode that once left stdio and HTTP advertising
/// different `claim_files` schemas.
pub struct ServerStatusFields {
    pub transport: Option<String>,
    pub port: Option<u16>,
    pub started_at: SystemTime,
    pub last_sync_at: SystemTime,
    pub last_error: Option<String>,
    pub repo_count: usize,
    pub workspace_count: usize,
}

/// Render the `get_server_status` payload. The single source of the
/// wire shape; both transports call this.
pub fn render_server_status(f: ServerStatusFields) -> serde_json::Value {
    use crate::server::build_info;
    serde_json::json!({
        "pid": std::process::id(),
        "version": build_info::VERSION,
        "git_sha": build_info::GIT_SHA,
        "binary_mtime_unix": build_info::binary_mtime_unix(),
        "binary_is_stale": build_info::binary_is_stale(),
        "transport": f.transport,
        // Null under stdio: reporting a port when nothing is listening
        // sends an agent to a dashboard that isn't there.
        "port": match (f.transport.as_deref(), f.port) {
            (Some("http"), Some(p)) => serde_json::Value::from(p),
            _ => serde_json::Value::Null,
        },
        "started_at": crate::server::time::unix_secs(f.started_at),
        "last_sync_at": crate::server::time::unix_secs(f.last_sync_at),
        "last_error": f.last_error,
        "repo_count": f.repo_count,
        "workspace_count": f.workspace_count,
    })
}

pub fn get_server_status(server: &LainServer) -> serde_json::Value {
    render_server_status(ServerStatusFields {
        transport: server.transport().map(|t| match t {
            crate::server::ingest::config::Transport::Stdio => "stdio".to_string(),
            crate::server::ingest::config::Transport::Http => "http".to_string(),
        }),
        port: server.port(),
        started_at: server.started_at(),
        last_sync_at: server.last_sync_at(),
        last_error: server.last_error(),
        repo_count: server.repo_count(),
        workspace_count: server.workspace_count(),
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
    //! One builder, two transports. `render_server_status` is now the
    //! only place the payload's shape is declared; both the
    //! single-workspace path and `mcp::handler::HandlerStatus` call it.
    //!
    //! This pins the wire shape. It began life as a drift guard between
    //! two hand-written builders that had already diverged — the HTTP
    //! one was serving a payload without the build-identity fields —
    //! which is the same failure mode that once left stdio and HTTP
    //! advertising different `claim_files` schemas.

    /// The keys the payload must carry. A field added without updating
    /// this fails here rather than in an agent's session weeks later.
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
            "get_server_status wire shape changed; update EXPECTED_KEYS deliberately"
        );
    }
}
