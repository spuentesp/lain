//! Unix socket signal path: CLI → server.
//!
//! The CLI writes `repos.yaml` / `workspaces.yaml` atomically, then
//! opens a Unix domain socket at
//! `~/.local/lain/run/<repos-stem>.sock` and writes `"reload\n"`. The
//! server listens on the same path; on receipt it asks the
//! `ReloadBus` to schedule a rebuild.
//!
//! If the socket doesn't exist (server isn't running) `signal_reload`
//! is a no-op — the YAML file was already saved atomically, so a
//! later server start will pick up the new contents naturally.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::net::UnixListener;

/// Compute the socket path the server is expected to be listening on
/// for `repos_yaml`. Always under `crate::config::run_dir()`, named
/// after the file stem of `repos_yaml`.
pub fn socket_path_for(repos_yaml: &Path) -> PathBuf {
    let stem = repos_yaml
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "default".to_string());
    crate::config::run_dir().join(format!("{stem}.sock"))
}

/// Tell the server (if running) to reload. Returns `Ok(())` whether
/// or not the server is up — a missing socket is not an error.
pub fn signal_reload(repos_yaml: &Path) -> anyhow::Result<()> {
    let sock = socket_path_for(repos_yaml);
    if !sock.exists() {
        // Server not running. The YAML file was already written
        // atomically, so a later server start picks it up.
        return Ok(());
    }
    let mut stream = std::os::unix::net::UnixStream::connect(&sock)?;
    stream.write_all(b"reload\n")?;
    Ok(())
}

/// Spawn a Unix socket listener at `path`. On receipt of `"reload\n"`,
/// the listener calls `bus.request_reload()`. The spawned task runs
/// until the listener is dropped (server shutdown). The caller
/// receives the listener's bound path so it can log or clean it up.
///
/// On Unix only — Windows would need a different transport.
#[cfg(unix)]
pub async fn spawn_signal_listener_at(
    path: &Path,
    bus: Arc<crate::server::reload::ReloadBus>,
) -> anyhow::Result<PathBuf> {
    use anyhow::Context;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    // Best-effort cleanup of any stale socket file from a previous run.
    let _ = tokio::fs::remove_file(path).await;
    let listener = UnixListener::bind(path)
        .with_context(|| format!("bind {}", path.display()))?;
    let bound = path.to_path_buf();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((mut stream, _)) => {
                    let mut buf = [0u8; 16];
                    match stream.read(&mut buf).await {
                        Ok(n) if &buf[..n] == b"reload\n" => {
                            if let Err(e) = bus.request_reload() {
                                tracing::warn!(
                                    "signal listener: bus.request_reload() failed: {e}"
                                );
                            }
                        }
                        Ok(_) => {
                            // Unknown command — ignore silently.
                        }
                        Err(e) => {
                            tracing::warn!(
                                "signal listener: read error: {e}"
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("signal listener: accept error: {e}");
                }
            }
        }
    });
    Ok(bound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_for_uses_repos_yaml_stem() {
        let tmp = tempfile::tempdir().unwrap();
        let repos = tmp.path().join("my-repos.yaml");
        let sock = socket_path_for(&repos);
        // The parent must be the run_dir, the filename ends in `.sock`.
        let name = sock.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(name, "my-repos.sock");
        assert!(sock.parent().is_some());
    }

    #[test]
    fn socket_path_for_defaults_when_no_stem() {
        let path = socket_path_for(Path::new("/"));
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        // No stem → default.sock.
        assert_eq!(name, "default.sock");
    }

    #[test]
    fn signal_reload_is_noop_when_socket_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let repos = tmp.path().join("repos.yaml");
        // No socket exists; signal_reload should be a clean Ok(()).
        assert!(signal_reload(&repos).is_ok());
    }

    /// End-to-end: spawn a listener task via the production
    /// `spawn_signal_listener_at`, signal reload, verify the bus
    /// receives the request.
    #[tokio::test(flavor = "current_thread")]
    async fn signal_listener_forwards_to_bus() {
        use tokio::io::AsyncWriteExt;

        // Use an explicit socket path in a tempdir (bypassing the
        // global `run_dir`) so the test doesn't collide with other
        // concurrent tests or a real server.
        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join("test.sock");
        let _ = std::fs::remove_file(&sock_path);

        let bus = std::sync::Arc::new(crate::server::reload::ReloadBus::new());
        let mut sub = bus.subscribe();
        spawn_signal_listener_at(&sock_path, std::sync::Arc::clone(&bus))
            .await
            .expect("spawn_signal_listener_at");

        // Give the listener a moment to start accepting.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        stream.write_all(b"reload\n").await.unwrap();

        let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if sub.try_recv().is_ok() {
                    return true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(result.unwrap_or(false), "expected reload request within 2s");
    }
}