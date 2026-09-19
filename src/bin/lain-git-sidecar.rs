//! Bug #2 sidecar prototype — child process.
//!
//! Hosts a `git2::Repository` and answers libgit2 calls from the
//! parent over a Unix domain socket. Throwaway prototype for the
//! 2026-09-18 Tauri federation postmortem's "sidecar process for
//! libgit2" investigation (Bug #2 root cause).
//!
//! Wire protocol: see `crate::sidecar_proto`.
//!
//! Usage:
//!
//! ```text
//! lain-git-sidecar <repo-path> <socket-path>
//! ```
//!
//! Listens on `<socket-path>`, serves one connection at a time
//! (single-threaded for the prototype — concurrent connections
//! would need threading + per-connection `git2::Repository` handles
//! which is out of scope here), exits on `Request::Shutdown` or
//! EOF.

use lain::git::{
    ChangeType as GitChangeType, CoChangePair as GitCoChangePair, CommitInfo as GitCommitInfo,
    FileChange as GitFileChange, GitSensor, RepoIdentity as GitRepoIdentity,
};
use lain::sidecar_proto::{
    read_frame, write_frame, ChangeType, CoChangePair, CommitInfo, FileChange, RepoIdentity,
    Request, Response, PROTOCOL_VERSION,
};

use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

#[cfg(target_os = "linux")]
fn configure_parent_death_signal() {
    extern "C" {
        fn prctl(
            option: std::os::raw::c_int,
            arg2: std::os::raw::c_ulong,
            arg3: std::os::raw::c_ulong,
            arg4: std::os::raw::c_ulong,
            arg5: std::os::raw::c_ulong,
        ) -> std::os::raw::c_int;
    }
    const PR_SET_PDEATHSIG: std::os::raw::c_int = 1;
    const SIGTERM: std::os::raw::c_ulong = 15;
    unsafe {
        let ret = prctl(PR_SET_PDEATHSIG, SIGTERM, 0, 0, 0);
        if ret != 0 {
            eprintln!(
                "[lain-git-sidecar:{}] warning: failed to set PR_SET_PDEATHSIG (errno: {})",
                std::process::id(),
                std::io::Error::last_os_error()
            );
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn configure_parent_death_signal() {}

#[cfg(unix)]
mod signals {
    use std::ffi::CString;
    use std::sync::atomic::{AtomicPtr, Ordering};

    static SOCKET_C_STR: AtomicPtr<std::os::raw::c_char> = AtomicPtr::new(std::ptr::null_mut());

    extern "C" {
        fn signal(sig: std::os::raw::c_int, handler: extern "C" fn(std::os::raw::c_int)) -> usize;
        fn unlink(pathname: *const std::os::raw::c_char) -> std::os::raw::c_int;
        fn _exit(status: std::os::raw::c_int) -> !;
    }

    extern "C" fn handle_signal(sig: std::os::raw::c_int) {
        let ptr = SOCKET_C_STR.load(Ordering::SeqCst);
        if !ptr.is_null() {
            unsafe {
                unlink(ptr);
                _exit(128 + sig);
            }
        }
        unsafe {
            _exit(1);
        }
    }

    pub fn register_socket_cleanup(path: &std::path::Path) {
        if let Ok(c_path) = CString::new(path.as_os_str().as_encoded_bytes()) {
            let leaked = c_path.into_raw();
            SOCKET_C_STR.store(leaked, Ordering::SeqCst);
            const SIGINT: std::os::raw::c_int = 2;
            const SIGTERM: std::os::raw::c_int = 15;
            unsafe {
                signal(SIGINT, handle_signal);
                signal(SIGTERM, handle_signal);
            }
        }
    }

    pub fn unregister_socket_cleanup() {
        let ptr = SOCKET_C_STR.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !ptr.is_null() {
            unsafe {
                let _ = CString::from_raw(ptr);
            }
        }
    }
}

struct SocketCleaner<'a>(&'a Path);
impl<'a> Drop for SocketCleaner<'a> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0);
        #[cfg(unix)]
        signals::unregister_socket_cleanup();
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    configure_parent_death_signal();

    let mut args = std::env::args().skip(1);
    let repo_path_arg = args
        .next()
        .ok_or("usage: lain-git-sidecar <repo-path> <socket-path>")?;
    let socket_path_arg = args
        .next()
        .ok_or("usage: lain-git-sidecar <repo-path> <socket-path>")?;

    let repo_path = dunce::canonicalize(&repo_path_arg).map_err(|e| {
        format!(
            "[lain-git-sidecar:{}] failed to canonicalize repo path '{}': {e}",
            std::process::id(),
            repo_path_arg
        )
    })?;
    let socket_path = Path::new(&socket_path_arg);

    let sensor = GitSensor::new(&repo_path)?;

    // Clean up any stale socket file at the path — if the previous
    // child died without unlinking, the bind would fail.
    let _ = std::fs::remove_file(socket_path);

    #[cfg(unix)]
    signals::register_socket_cleanup(socket_path);
    let _cleaner = SocketCleaner(socket_path);

    let listener = UnixListener::bind(socket_path)?;
    eprintln!(
        "[lain-git-sidecar:{}] listening on {}",
        std::process::id(),
        socket_path.display()
    );

    // Single connection at a time for the prototype. Production
    // would want a thread pool + per-connection GitSensor (or a
    // thread-safe Arc<Mutex<GitSensor>> shared across connections).
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => match handle_connection(stream, &sensor) {
                Ok(true) => break,
                Ok(false) => {}
                Err(e) => {
                    eprintln!(
                        "[lain-git-sidecar:{}] connection error: {e}",
                        std::process::id()
                    );
                }
            },
            Err(e) => {
                eprintln!(
                    "[lain-git-sidecar:{}] accept error: {e}",
                    std::process::id()
                );
            }
        }
    }

    Ok(())
}

fn handle_connection(
    mut stream: UnixStream,
    sensor: &GitSensor,
) -> Result<bool, Box<dyn std::error::Error>> {
    // Handshake check: first frame must be Request::Handshake matching PROTOCOL_VERSION.
    let first_req: Request = match read_frame(&mut stream) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(false),
        Err(e) => return Err(Box::new(e)),
    };

    match first_req {
        Request::Handshake { version } if version == PROTOCOL_VERSION => {
            write_frame(
                &mut stream,
                &Response::HandshakeAck {
                    version: PROTOCOL_VERSION,
                },
            )?;
            stream.flush()?;
        }
        Request::Handshake { version } => {
            let nack = Response::HandshakeNack {
                expected: PROTOCOL_VERSION,
                received: version,
                reason: format!(
                    "protocol version mismatch: expected {}, got {}",
                    PROTOCOL_VERSION, version
                ),
            };
            let _ = write_frame(&mut stream, &nack);
            let _ = stream.flush();
            return Err(format!(
                "handshake version mismatch: expected {}, got {}",
                PROTOCOL_VERSION, version
            )
            .into());
        }
        other => {
            let nack = Response::HandshakeNack {
                expected: PROTOCOL_VERSION,
                received: 0,
                reason: "initial message must be Request::Handshake".to_string(),
            };
            let _ = write_frame(&mut stream, &nack);
            let _ = stream.flush();
            return Err(format!("expected handshake as first frame, got: {:?}", other).into());
        }
    }

    loop {
        let req: Request = match read_frame(&mut stream) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                // Parent closed the connection. Clean shutdown.
                return Ok(false);
            }
            Err(e) => return Err(Box::new(e)),
        };
        let resp = dispatch(&req, sensor);
        if matches!(req, Request::Shutdown) {
            write_frame(&mut stream, &resp)?;
            stream.flush()?;
            return Ok(true);
        }
        write_frame(&mut stream, &resp)?;
        stream.flush()?;
    }
}

fn dispatch(req: &Request, sensor: &GitSensor) -> Response {
    match req {
        Request::Handshake { version } => {
            if *version == PROTOCOL_VERSION {
                Response::HandshakeAck {
                    version: PROTOCOL_VERSION,
                }
            } else {
                Response::HandshakeNack {
                    expected: PROTOCOL_VERSION,
                    received: *version,
                    reason: "protocol version mismatch".to_string(),
                }
            }
        }
        Request::GetLatestCommitInfo => match sensor.get_latest_commit_info() {
            Ok((commit, timestamp)) => Response::LatestCommitInfo { commit, timestamp },
            Err(e) => Response::Error(e.to_string()),
        },
        Request::GetChangedFilesSince { since_hash } => {
            match sensor.get_changed_files_since(since_hash) {
                Ok(paths) => Response::ChangedFiles(paths),
                Err(e) => Response::Error(e.to_string()),
            }
        }
        Request::GetAllTrackedFiles => match sensor.get_all_tracked_files() {
            Ok(paths) => Response::TrackedFiles(paths),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::AnalyzeCoChanges {
            window,
            min_pair,
            max_files,
        } => match sensor.analyze_co_changes(*window, *min_pair, *max_files) {
            Ok(pairs) => {
                Response::CoChanges(pairs.into_iter().map(git_cochange_to_proto).collect())
            }
            Err(e) => Response::Error(e.to_string()),
        },
        Request::GetUncommittedChanges => match sensor.get_uncommitted_changes() {
            Ok(changes) => Response::UncommittedChanges(
                changes.into_iter().map(git_filechange_to_proto).collect(),
            ),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::IsIgnored { path } => match sensor.is_ignored(path) {
            Ok(ignored) => Response::IsIgnored(ignored),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::GetFileDiff { path } => match sensor.get_file_diff(path) {
            Ok(diff) => Response::FileDiff(diff),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::GetCurrentBranch => match sensor.get_current_branch() {
            Ok(branch) => Response::CurrentBranch(branch),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::GetCommitHistory { count } => match sensor.get_commit_history(*count) {
            Ok(commits) => {
                Response::CommitHistory(commits.into_iter().map(git_commit_to_proto).collect())
            }
            Err(e) => Response::Error(e.to_string()),
        },
        Request::GetRepoIdentity => match sensor.get_repo_identity() {
            Ok(id) => Response::RepoIdentity(id.map(git_identity_to_proto)),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::GetNewCommitsSince { since_hash } => {
            match sensor.get_new_commits_since(since_hash) {
                Ok(commits) => {
                    Response::CommitHistory(commits.into_iter().map(git_commit_to_proto).collect())
                }
                Err(e) => Response::Error(e.to_string()),
            }
        }
        Request::Shutdown => Response::Ok,
    }
}

fn git_cochange_to_proto(p: GitCoChangePair) -> CoChangePair {
    CoChangePair {
        file1: p.file1,
        file2: p.file2,
        co_change_count: p.co_change_count,
    }
}

fn git_filechange_to_proto(c: GitFileChange) -> FileChange {
    FileChange {
        path: c.path,
        change_type: match c.change_type {
            GitChangeType::Added => ChangeType::Added,
            GitChangeType::Modified => ChangeType::Modified,
            GitChangeType::Deleted => ChangeType::Deleted,
        },
        staged: c.staged,
    }
}

fn git_commit_to_proto(c: GitCommitInfo) -> CommitInfo {
    CommitInfo {
        id: c.id,
        message: c.message,
        files: c.files,
        time: c.time,
    }
}

fn git_identity_to_proto(id: GitRepoIdentity) -> RepoIdentity {
    RepoIdentity {
        owner: id.owner,
        name: id.name,
    }
}
