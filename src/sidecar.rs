//! Bug #2 libgit2 Sidecar Client and Process Supervisor.
//!
//! Hosts [`SidecarGitSensor`], which spawns and supervises the `lain-git-sidecar`
//! child process over a Unix domain socket, enforcing call timeouts,
//! respawn budgets (max 3 retries in 30 seconds), and automatic transparent recovery.

use crate::error::LainError;
use crate::git::{ChangeType, CoChangePair, CommitInfo, FileChange, RepoIdentity};
use crate::sidecar_proto::{
    read_frame, write_frame, ChangeType as ProtoChangeType, CoChangePair as ProtoCoChangePair,
    CommitInfo as ProtoCommitInfo, FileChange as ProtoFileChange,
    RepoIdentity as ProtoRepoIdentity, Request, Response, PROTOCOL_VERSION,
};

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(2);
const RESPAWN_WINDOW: Duration = Duration::from_secs(30);
const MAX_RESPAWNS_PER_WINDOW: usize = 3;

/// Health and diagnostic snapshot of the sidecar daemon.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SidecarHealth {
    pub alive: bool,
    pub child_pid: Option<u32>,
    pub consecutive_failures: u32,
    pub respawns_in_window: usize,
    pub last_call_duration_us: u64,
}

/// Out-of-process GitSensor backed by IPC to `lain-git-sidecar`.
pub struct SidecarGitSensor {
    workspace: PathBuf,
    inner: Arc<Mutex<SidecarInner>>,
}

impl SidecarGitSensor {
    /// Connect to or spawn a `lain-git-sidecar` child for the given workspace.
    pub fn new(workspace: &Path) -> Result<Self, LainError> {
        Self::new_with_options(workspace, None, DEFAULT_CALL_TIMEOUT)
    }

    /// Connect to or spawn a `lain-git-sidecar` with an explicit binary path and call timeout.
    pub fn new_with_options(
        workspace: &Path,
        bin_path: Option<PathBuf>,
        call_timeout: Duration,
    ) -> Result<Self, LainError> {
        let workspace_canon = dunce::canonicalize(workspace).map_err(|e| {
            LainError::Git(format!(
                "failed to canonicalize workspace '{}': {e}",
                workspace.display()
            ))
        })?;

        // Fast pre-flight check: workspace must be a valid git repository before spawning
        git2::Repository::open(&workspace_canon)?;

        let mut inner = SidecarInner {
            workspace: workspace_canon.clone(),
            child: None,
            stream: None,
            socket_path: generate_socket_path(),
            respawn_history: VecDeque::new(),
            consecutive_failures: 0,
            last_call_duration: Duration::ZERO,
            call_timeout,
            custom_bin_path: bin_path,
            is_initial_boot: true,
        };

        // Initial spawn and connection check
        inner.ensure_connected()?;

        Ok(Self {
            workspace: workspace_canon,
            inner: Arc::new(Mutex::new(inner)),
        })
    }

    /// Current repository workspace path.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Path to the Unix domain socket currently used for IPC.
    pub fn socket_path(&self) -> PathBuf {
        self.inner.lock().socket_path.clone()
    }

    /// Health inspection snapshot.
    pub fn health(&self) -> SidecarHealth {
        self.inner.lock().health()
    }

    /// "What's HEAD right now?" Returns (commit_hash, timestamp).
    pub fn get_latest_commit_info(&self) -> Result<(String, i64), LainError> {
        self.inner
            .lock()
            .call(Request::GetLatestCommitInfo, |resp| match resp {
                Response::LatestCommitInfo { commit, timestamp } => Ok((commit, timestamp)),
                Response::Error(msg) => Err(LainError::Git(msg)),
                other => Err(LainError::Git(format!("unexpected response: {:?}", other))),
            })
    }

    /// "List files changed since this commit hash."
    pub fn get_changed_files_since(&self, since: &str) -> Result<Vec<PathBuf>, LainError> {
        self.inner.lock().call(
            Request::GetChangedFilesSince {
                since_hash: since.to_string(),
            },
            |resp| match resp {
                Response::ChangedFiles(files) => Ok(files),
                Response::Error(msg) => Err(LainError::Git(msg)),
                other => Err(LainError::Git(format!("unexpected response: {:?}", other))),
            },
        )
    }

    /// "List every tracked file."
    pub fn get_all_tracked_files(&self) -> Result<Vec<PathBuf>, LainError> {
        self.inner
            .lock()
            .call(Request::GetAllTrackedFiles, |resp| match resp {
                Response::TrackedFiles(files) => Ok(files),
                Response::Error(msg) => Err(LainError::Git(msg)),
                other => Err(LainError::Git(format!("unexpected response: {:?}", other))),
            })
    }

    /// "Compute co-change pairs from the last `window` commits."
    pub fn analyze_co_changes(
        &self,
        window: usize,
        min_pair: usize,
        max_files: usize,
    ) -> Result<Vec<CoChangePair>, LainError> {
        self.inner.lock().call(
            Request::AnalyzeCoChanges {
                window,
                min_pair,
                max_files,
            },
            |resp| match resp {
                Response::CoChanges(pairs) => {
                    Ok(pairs.into_iter().map(proto_to_cochange).collect())
                }
                Response::Error(msg) => Err(LainError::Git(msg)),
                other => Err(LainError::Git(format!("unexpected response: {:?}", other))),
            },
        )
    }

    /// "List uncommitted changes (staged + unstaged + untracked)."
    pub fn get_uncommitted_changes(&self) -> Result<Vec<FileChange>, LainError> {
        self.inner
            .lock()
            .call(Request::GetUncommittedChanges, |resp| match resp {
                Response::UncommittedChanges(changes) => {
                    Ok(changes.into_iter().map(proto_to_filechange).collect())
                }
                Response::Error(msg) => Err(LainError::Git(msg)),
                other => Err(LainError::Git(format!("unexpected response: {:?}", other))),
            })
    }

    /// "Is this a valid Git repository with a working HEAD?"
    pub fn is_valid(&self) -> bool {
        self.get_latest_commit_info().is_ok()
    }

    /// "Is this path ignored by .gitignore rules?"
    pub fn is_ignored(&self, path: &Path) -> Result<bool, LainError> {
        self.inner.lock().call(
            Request::IsIgnored {
                path: path.to_path_buf(),
            },
            |resp| match resp {
                Response::IsIgnored(ignored) => Ok(ignored),
                Response::Error(msg) => Err(LainError::Git(msg)),
                other => Err(LainError::Git(format!("unexpected response: {:?}", other))),
            },
        )
    }

    /// "Get the latest commit hash."
    pub fn get_latest_commit(&self) -> Result<String, LainError> {
        self.get_latest_commit_info().map(|(c, _)| c)
    }

    /// "Get diff for a specific file."
    pub fn get_file_diff(&self, path: &Path) -> Result<String, LainError> {
        self.inner.lock().call(
            Request::GetFileDiff {
                path: path.to_path_buf(),
            },
            |resp| match resp {
                Response::FileDiff(diff) => Ok(diff),
                Response::Error(msg) => Err(LainError::Git(msg)),
                other => Err(LainError::Git(format!("unexpected response: {:?}", other))),
            },
        )
    }

    /// "Get the current branch name."
    pub fn get_current_branch(&self) -> Result<String, LainError> {
        self.inner
            .lock()
            .call(Request::GetCurrentBranch, |resp| match resp {
                Response::CurrentBranch(branch) => Ok(branch),
                Response::Error(msg) => Err(LainError::Git(msg)),
                other => Err(LainError::Git(format!("unexpected response: {:?}", other))),
            })
    }

    /// "Get commit history for co-change or timeline analysis."
    pub fn get_commit_history(&self, count: usize) -> Result<Vec<CommitInfo>, LainError> {
        self.inner
            .lock()
            .call(Request::GetCommitHistory { count }, |resp| match resp {
                Response::CommitHistory(commits) => {
                    Ok(commits.into_iter().map(proto_to_commitinfo).collect())
                }
                Response::Error(msg) => Err(LainError::Git(msg)),
                other => Err(LainError::Git(format!("unexpected response: {:?}", other))),
            })
    }

    /// "Get repository identity (owner, name) from git remote."
    pub fn get_repo_identity(&self) -> Result<Option<RepoIdentity>, LainError> {
        self.inner
            .lock()
            .call(Request::GetRepoIdentity, |resp| match resp {
                Response::RepoIdentity(id) => Ok(id.map(proto_to_repoidentity)),
                Response::Error(msg) => Err(LainError::Git(msg)),
                other => Err(LainError::Git(format!("unexpected response: {:?}", other))),
            })
    }

    /// "Get commits newer than the given commit hash."
    pub fn get_new_commits_since(&self, since_hash: &str) -> Result<Vec<CommitInfo>, LainError> {
        self.inner.lock().call(
            Request::GetNewCommitsSince {
                since_hash: since_hash.to_string(),
            },
            |resp| match resp {
                Response::CommitHistory(commits) => {
                    Ok(commits.into_iter().map(proto_to_commitinfo).collect())
                }
                Response::Error(msg) => Err(LainError::Git(msg)),
                other => Err(LainError::Git(format!("unexpected response: {:?}", other))),
            },
        )
    }
}

impl std::fmt::Debug for SidecarGitSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SidecarGitSensor")
            .field("workspace", &self.workspace)
            .finish()
    }
}

struct SidecarInner {
    workspace: PathBuf,
    child: Option<Child>,
    stream: Option<UnixStream>,
    socket_path: PathBuf,
    respawn_history: VecDeque<Instant>,
    consecutive_failures: u32,
    last_call_duration: Duration,
    call_timeout: Duration,
    custom_bin_path: Option<PathBuf>,
    is_initial_boot: bool,
}

impl SidecarInner {
    fn health(&mut self) -> SidecarHealth {
        let now = Instant::now();
        let respawns = self
            .respawn_history
            .iter()
            .filter(|t| now.duration_since(**t) <= RESPAWN_WINDOW)
            .count();

        if let Some(child) = self.child.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                self.stream = None;
            }
        }

        SidecarHealth {
            alive: self.stream.is_some() && self.consecutive_failures == 0,
            child_pid: self.child.as_ref().map(|c| c.id()),
            consecutive_failures: self.consecutive_failures,
            respawns_in_window: respawns,
            last_call_duration_us: self.last_call_duration.as_micros() as u64,
        }
    }

    fn call<F, R>(&mut self, req: Request, parse: F) -> Result<R, LainError>
    where
        F: Fn(Response) -> Result<R, LainError>,
    {
        self.ensure_connected()?;

        let started = Instant::now();
        let write_res = self.write_request(&req);
        let read_res = match write_res {
            Ok(()) => self.read_response(),
            Err(e) => Err(e),
        };

        match read_res {
            Ok(resp) => {
                self.last_call_duration = started.elapsed();
                self.consecutive_failures = 0;
                parse(resp)
            }
            Err(e) => {
                warn!(
                    "[lain-git-sidecar] call failed ({e}); forcing child respawn and retrying call"
                );
                self.force_teardown();
                self.consecutive_failures += 1;

                // Attempt single retry after respawn
                self.ensure_connected()?;
                let retry_started = Instant::now();
                let retry_res = (|| -> Result<Response, LainError> {
                    self.write_request(&req)?;
                    self.read_response()
                })();

                match retry_res {
                    Ok(resp) => {
                        self.last_call_duration = retry_started.elapsed();
                        self.consecutive_failures = 0;
                        parse(resp)
                    }
                    Err(retry_err) => {
                        self.force_teardown();
                        self.consecutive_failures += 1;
                        Err(retry_err)
                    }
                }
            }
        }
    }

    fn write_request(&mut self, req: &Request) -> Result<(), LainError> {
        let stream = self.stream.as_mut().ok_or_else(|| {
            LainError::Unavailable("git sidecar stream is disconnected".to_string())
        })?;

        stream
            .set_write_timeout(Some(self.call_timeout))
            .map_err(|e| LainError::Io(format!("set write timeout: {e}")))?;

        write_frame(stream, req).map_err(|e| LainError::Io(format!("write request frame: {e}")))?;
        stream
            .flush()
            .map_err(|e| LainError::Io(format!("flush request frame: {e}")))?;
        Ok(())
    }

    fn read_response(&mut self) -> Result<Response, LainError> {
        let stream = self.stream.as_mut().ok_or_else(|| {
            LainError::Unavailable("git sidecar stream is disconnected".to_string())
        })?;

        stream
            .set_read_timeout(Some(self.call_timeout))
            .map_err(|e| LainError::Io(format!("set read timeout: {e}")))?;

        read_frame(stream).map_err(|e| LainError::Io(format!("read response frame: {e}")))
    }

    fn ensure_connected(&mut self) -> Result<(), LainError> {
        if self.stream.is_some() {
            return Ok(());
        }

        if self.is_initial_boot {
            self.is_initial_boot = false;
            return self.spawn_child_and_connect();
        }

        // Enforce respawn budget
        let now = Instant::now();
        while let Some(front) = self.respawn_history.front() {
            if now.duration_since(*front) > RESPAWN_WINDOW {
                self.respawn_history.pop_front();
            } else {
                break;
            }
        }

        if self.respawn_history.len() >= MAX_RESPAWNS_PER_WINDOW {
            return Err(LainError::Unavailable(format!(
                "git sidecar down: respawn budget exceeded ({} respawns in {:?})",
                MAX_RESPAWNS_PER_WINDOW, RESPAWN_WINDOW
            )));
        }

        self.spawn_child_and_connect()?;
        self.respawn_history.push_back(now);
        Ok(())
    }

    fn spawn_child_and_connect(&mut self) -> Result<(), LainError> {
        self.force_teardown();
        self.socket_path = generate_socket_path();

        let bin_path = resolve_sidecar_binary(self.custom_bin_path.as_deref())?;
        debug!(
            "[lain-git-sidecar] spawning {:?} for repo {:?}",
            bin_path, self.workspace
        );

        let child = Command::new(&bin_path)
            .arg(&self.workspace)
            .arg(&self.socket_path)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                LainError::Unavailable(format!(
                    "failed to spawn sidecar binary {:?}: {e}",
                    bin_path
                ))
            })?;

        self.child = Some(child);

        // Connect with retry timeout up to 2 seconds
        let connect_timeout = Duration::from_secs(2);
        let start = Instant::now();
        let stream = loop {
            if let Some(child) = self.child.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    self.force_teardown();
                    return Err(LainError::Unavailable(format!(
                        "git sidecar child exited prematurely with status {status}"
                    )));
                }
            }
            match UnixStream::connect(&self.socket_path) {
                Ok(s) => break s,
                Err(e) if start.elapsed() < connect_timeout => {
                    std::thread::sleep(Duration::from_millis(25));
                    let _ = e;
                }
                Err(e) => {
                    self.force_teardown();
                    return Err(LainError::Unavailable(format!(
                        "timed out connecting to git sidecar socket '{:?}': {e}",
                        self.socket_path
                    )));
                }
            }
        };

        self.stream = Some(stream);

        // Execute protocol handshake immediately
        if let Err(e) = self.perform_handshake() {
            self.force_teardown();
            return Err(e);
        }

        info!(
            "[lain-git-sidecar] connected to child daemon at {:?}",
            self.socket_path
        );
        Ok(())
    }

    fn perform_handshake(&mut self) -> Result<(), LainError> {
        let req = Request::Handshake {
            version: PROTOCOL_VERSION,
        };
        self.write_request(&req)?;
        match self.read_response()? {
            Response::HandshakeAck { version } => {
                if version != PROTOCOL_VERSION {
                    return Err(LainError::Fatal(format!(
                        "git sidecar handshake version mismatch: expected {}, got {}",
                        PROTOCOL_VERSION, version
                    )));
                }
                Ok(())
            }
            Response::HandshakeNack {
                expected,
                received,
                reason,
            } => Err(LainError::Fatal(format!(
                "git sidecar handshake rejected: expected {expected}, received {received}, reason: {reason}"
            ))),
            other => Err(LainError::Fatal(format!(
                "expected HandshakeAck from sidecar, got {:?}",
                other
            ))),
        }
    }

    fn force_teardown(&mut self) {
        if let Some(mut stream) = self.stream.take() {
            let _ = stream.set_write_timeout(Some(Duration::from_millis(50)));
            let _ = write_frame(&mut stream, &Request::Shutdown);
            let _ = stream.flush();
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl Drop for SidecarInner {
    fn drop(&mut self) {
        self.force_teardown();
    }
}

fn generate_socket_path() -> PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    path.push(format!(
        "lain-sidecar-{}-{}.sock",
        std::process::id(),
        nanos
    ));
    path
}

fn resolve_sidecar_binary(custom: Option<&Path>) -> Result<PathBuf, LainError> {
    if let Some(p) = custom {
        if p.exists() {
            return Ok(p.to_path_buf());
        }
    }

    if let Some(val) = std::env::var_os("LAIN_GIT_SIDECAR_BIN") {
        let p = PathBuf::from(val);
        if p.exists() {
            return Ok(p);
        }
    }

    if let Some(val) = std::env::var_os("CARGO_BIN_EXE_lain-git-sidecar") {
        let p = PathBuf::from(val);
        if p.exists() {
            return Ok(p);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        let sidecar_name = if cfg!(windows) {
            "lain-git-sidecar.exe"
        } else {
            "lain-git-sidecar"
        };
        let sibling = exe.with_file_name(sidecar_name);
        if sibling.exists() {
            return Ok(sibling);
        }
        if let Some(parent) = exe.parent() {
            if parent.file_name().and_then(|f| f.to_str()) == Some("deps") {
                if let Some(target_dir) = parent.parent() {
                    let sidecar = target_dir.join(sidecar_name);
                    if sidecar.exists() {
                        return Ok(sidecar);
                    }
                }
            }
        }
    }

    let sidecar_name = if cfg!(windows) {
        "lain-git-sidecar.exe"
    } else {
        "lain-git-sidecar"
    };

    if let Ok(path) = which::which(sidecar_name) {
        return Ok(path);
    }

    // Check development target directories
    for dir in &["target/debug", "target/release"] {
        let p = Path::new(dir).join(sidecar_name);
        if p.exists() {
            return Ok(dunce::canonicalize(&p).unwrap_or(p));
        }
    }

    Err(LainError::Unavailable(
        "could not find 'lain-git-sidecar' binary; ensure it is built or set LAIN_GIT_SIDECAR_BIN"
            .to_string(),
    ))
}

fn proto_to_cochange(p: ProtoCoChangePair) -> CoChangePair {
    CoChangePair {
        file1: p.file1,
        file2: p.file2,
        co_change_count: p.co_change_count,
    }
}

fn proto_to_filechange(c: ProtoFileChange) -> FileChange {
    FileChange {
        path: c.path,
        change_type: match c.change_type {
            ProtoChangeType::Added => ChangeType::Added,
            ProtoChangeType::Modified => ChangeType::Modified,
            ProtoChangeType::Deleted => ChangeType::Deleted,
        },
        staged: c.staged,
    }
}

fn proto_to_commitinfo(c: ProtoCommitInfo) -> CommitInfo {
    CommitInfo {
        id: c.id,
        message: c.message,
        files: c.files,
        time: c.time,
    }
}

fn proto_to_repoidentity(i: ProtoRepoIdentity) -> RepoIdentity {
    RepoIdentity {
        owner: i.owner,
        name: i.name,
    }
}
