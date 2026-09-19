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
    ChangeType as GitChangeType, CoChangePair as GitCoChangePair, FileChange as GitFileChange,
    GitSensor,
};
use lain::sidecar_proto::{
    read_frame, write_frame, ChangeType, CoChangePair, FileChange, Request, Response,
};

use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let repo_path = args
        .next()
        .ok_or("usage: lain-git-sidecar <repo-path> <socket-path>")?;
    let socket_path = args
        .next()
        .ok_or("usage: lain-git-sidecar <repo-path> <socket-path>")?;

    let sensor = GitSensor::new(Path::new(&repo_path))?;

    // Clean up any stale socket file at the path — if the previous
    // child died without unlinking, the bind would fail.
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)?;
    eprintln!("lain-git-sidecar: listening on {socket_path}");

    // Single connection at a time for the prototype. Production
    // would want a thread pool + per-connection GitSensor (or a
    // thread-safe Arc<Mutex<GitSensor>> shared across connections).
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(e) = handle_connection(stream, &sensor) {
                    eprintln!("lain-git-sidecar: connection error: {e}");
                }
            }
            Err(e) => {
                eprintln!("lain-git-sidecar: accept error: {e}");
            }
        }
    }

    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

fn handle_connection(
    mut stream: UnixStream,
    sensor: &GitSensor,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        let req: Request = match read_frame(&mut stream) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                // Parent closed the connection. Clean shutdown.
                return Ok(());
            }
            Err(e) => return Err(Box::new(e)),
        };
        let resp = dispatch(&req, sensor);
        if matches!(req, Request::Shutdown) {
            write_frame(&mut stream, &resp)?;
            stream.flush()?;
            return Ok(());
        }
        write_frame(&mut stream, &resp)?;
        stream.flush()?;
    }
}

fn dispatch(req: &Request, sensor: &GitSensor) -> Response {
    match req {
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
