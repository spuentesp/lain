//! Wire protocol for the Bug #2 sidecar prototype.
//!
//! The child (`lain-git-sidecar`) opens a `git2::Repository` and answers
//! libgit2 calls from the parent over a Unix domain socket. Each
//! message is a length-prefixed bincode frame:
//!
//! ```text
//! +--------+-------------------+--------+-------------------+
//! | u32 BE | bincode Request   | ...     |                   |
//! +--------+-------------------+--------+-------------------+
//! ```
//!
//! The parent sends one request, the child replies with one response.
//! Request IDs match for correlation.
//!
//! This module is shared by both binaries (`src/bin/lain-git-sidecar.rs`
//! and `src/bin/sidecar_bench.rs`). It's part of the lib so both can
//! `use crate::sidecar_proto::*;`. If the prototype decides no-go, rip
//! out the bin entries in Cargo.toml and delete this module.

/// 2: `HandshakeAck` carries the sidecar's build identity.
pub const PROTOCOL_VERSION: u32 = 2;

/// This binary's build identity (`0.7.4 (f64cf6b)`), as the sidecar
/// reports it in `HandshakeAck`. Lain refuses a sidecar from another
/// build: an older one found on `$PATH` answered the same protocol with
/// different behavior, and the graph silently missed files.
pub fn build_id() -> String {
    format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("LAIN_GIT_SHA"))
}

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Request from parent to child. Tagged enum so the dispatcher can
/// route by variant without an explicit method id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Request {
    /// Initial handshake to verify protocol compatibility. Must be sent
    /// immediately upon connection before any operational requests.
    Handshake { version: u32 },
    /// "What's HEAD right now?" Returns the commit hash and the
    /// commit-time epoch seconds.
    GetLatestCommitInfo,
    /// "List files changed since this commit hash." Returns the
    /// tracked files touched by commits strictly newer than `since`.
    GetChangedFilesSince { since_hash: String },
    /// "List every tracked file." Returns every tracked file in the
    /// workdir (the index, after applying .gitignore rules).
    GetAllTrackedFiles,
    /// "Compute co-change pairs from the last `window` commits."
    /// Mirrors `GitSensor::analyze_co_changes` parameter order.
    AnalyzeCoChanges {
        window: usize,
        min_pair: usize,
        max_files: usize,
    },
    /// "List uncommitted changes (staged + unstaged + untracked)."
    GetUncommittedChanges,
    /// "Is this path ignored by .gitignore rules?"
    IsIgnored { path: PathBuf },
    /// "Get diff for a specific file."
    GetFileDiff { path: PathBuf },
    /// "Get the current branch name."
    GetCurrentBranch,
    /// "Get commit history for co-change or timeline analysis."
    GetCommitHistory { count: usize },
    /// "Get repository identity (owner, name) from git remote."
    GetRepoIdentity,
    /// "Get commits newer than the given commit hash."
    GetNewCommitsSince { since_hash: String },
    /// "Exit cleanly." The child replies with `Response::Ok` and
    /// drops the listening socket.
    Shutdown,
}

/// Response from child to parent. Successful variant carries the
/// method-specific payload; `Error` carries a human-readable string
/// (libgit2 error message, panic message, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Response {
    /// Acknowledges a successful handshake with the matching protocol version.
    HandshakeAck {
        version: u32,
        /// [`build_id`] of the sidecar binary.
        build: String,
    },
    /// Rejection of a handshake due to version mismatch or negotiation failure.
    HandshakeNack {
        expected: u32,
        received: u32,
        reason: String,
    },
    LatestCommitInfo {
        commit: String,
        timestamp: i64,
    },
    ChangedFiles(Vec<PathBuf>),
    TrackedFiles(Vec<PathBuf>),
    CoChanges(Vec<CoChangePair>),
    UncommittedChanges(Vec<FileChange>),
    IsIgnored(bool),
    FileDiff(String),
    CurrentBranch(String),
    CommitHistory(Vec<CommitInfo>),
    RepoIdentity(Option<RepoIdentity>),
    /// Acknowledges a `Shutdown`. The child exits after writing this.
    Ok,
    /// Method-specific error. The parent treats this as the libgit2
    /// equivalent of an `Err(LainError::Git(_))` and surfaces it to
    /// the federation readiness path the same way the in-process
    /// `GitSensor` would.
    Error(String),
}

/// Mirrors `crate::git::CoChangePair` for the wire. Duplicated to keep
/// this module dependency-free of the main crate's git module (the
/// child doesn't need anything else).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoChangePair {
    pub file1: String,
    pub file2: String,
    pub co_change_count: usize,
}

/// Mirrors `crate::git::FileChange` for the wire. Same rationale as
/// `CoChangePair`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileChange {
    pub path: PathBuf,
    pub change_type: ChangeType,
    pub staged: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChangeType {
    Added,
    Modified,
    Deleted,
}

/// Mirrors `crate::git::CommitInfo` for the wire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommitInfo {
    pub id: String,
    pub message: String,
    pub files: Vec<String>,
    pub time: i64,
}

/// Mirrors `crate::git::RepoIdentity` for the wire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoIdentity {
    pub owner: String,
    pub name: String,
}

/// Length-prefixed frame codec. Writes/reads a 4-byte big-endian
/// length followed by the bincode payload. Used by both parent and
/// child so the framing is symmetric.
pub fn write_frame<W: std::io::Write, T: Serialize>(w: &mut W, msg: &T) -> std::io::Result<()> {
    let bytes = bincode::serde::encode_to_vec(msg, bincode::config::standard())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let len = u32::try_from(bytes.len())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "frame too large"))?;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(&bytes)?;
    Ok(())
}

/// Read one length-prefixed frame. Returns the deserialized message
/// or an IO error if the connection drops or the frame is malformed.
pub fn read_frame<R: std::io::Read, T: for<'de> Deserialize<'de>>(r: &mut R) -> std::io::Result<T> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    let (msg, _) = bincode::serde::decode_from_slice(&payload, bincode::config::standard())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_handshake_roundtrip() {
        let req = Request::Handshake {
            version: PROTOCOL_VERSION,
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &req).expect("write_frame failed");

        let mut cursor = Cursor::new(buf);
        let decoded: Request = read_frame(&mut cursor).expect("read_frame failed");
        assert_eq!(req, decoded);

        let ack = Response::HandshakeAck {
            version: PROTOCOL_VERSION,
            build: build_id(),
        };
        let mut ack_buf = Vec::new();
        write_frame(&mut ack_buf, &ack).expect("write ack failed");
        let mut ack_cursor = Cursor::new(ack_buf);
        let decoded_ack: Response = read_frame(&mut ack_cursor).expect("read ack failed");
        assert_eq!(ack, decoded_ack);
    }

    #[test]
    fn test_handshake_nack_roundtrip() {
        let nack = Response::HandshakeNack {
            expected: PROTOCOL_VERSION,
            received: 999,
            reason: "unsupported protocol version 999".to_string(),
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &nack).expect("write nack failed");

        let mut cursor = Cursor::new(buf);
        let decoded: Response = read_frame(&mut cursor).expect("read nack failed");
        assert_eq!(nack, decoded);
    }

    #[test]
    fn test_frames_roundtrip() {
        let req = Request::GetChangedFilesSince {
            since_hash: "abc1234".to_string(),
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &req).unwrap();
        let decoded: Request = read_frame(&mut Cursor::new(buf)).unwrap();
        assert_eq!(req, decoded);

        let resp = Response::ChangedFiles(vec![PathBuf::from("foo/bar.rs")]);
        let mut resp_buf = Vec::new();
        write_frame(&mut resp_buf, &resp).unwrap();
        let decoded_resp: Response = read_frame(&mut Cursor::new(resp_buf)).unwrap();
        assert_eq!(resp, decoded_resp);

        let branch_resp = Response::CurrentBranch("main".to_string());
        let mut branch_buf = Vec::new();
        write_frame(&mut branch_buf, &branch_resp).unwrap();
        let decoded_branch: Response = read_frame(&mut Cursor::new(branch_buf)).unwrap();
        assert_eq!(branch_resp, decoded_branch);

        let commits_resp = Response::CommitHistory(vec![CommitInfo {
            id: "123".to_string(),
            message: "msg".to_string(),
            files: vec!["a.rs".to_string()],
            time: 123456,
        }]);
        let mut commits_buf = Vec::new();
        write_frame(&mut commits_buf, &commits_resp).unwrap();
        let decoded_commits: Response = read_frame(&mut Cursor::new(commits_buf)).unwrap();
        assert_eq!(commits_resp, decoded_commits);
    }
}
