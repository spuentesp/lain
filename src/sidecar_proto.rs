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

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Request from parent to child. Tagged enum so the dispatcher can
/// route by variant without an explicit method id.
#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
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
    /// "Exit cleanly." The child replies with `Response::Ok` and
    /// drops the listening socket.
    Shutdown,
}

/// Response from child to parent. Successful variant carries the
/// method-specific payload; `Error` carries a human-readable string
/// (libgit2 error message, panic message, etc.).
#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    LatestCommitInfo {
        commit: String,
        timestamp: i64,
    },
    ChangedFiles(Vec<PathBuf>),
    TrackedFiles(Vec<PathBuf>),
    CoChanges(Vec<CoChangePair>),
    UncommittedChanges(Vec<FileChange>),
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
#[derive(Debug, Serialize, Deserialize)]
pub struct CoChangePair {
    pub file1: String,
    pub file2: String,
    pub co_change_count: usize,
}

/// Mirrors `crate::git::FileChange` for the wire. Same rationale as
/// `CoChangePair`.
#[derive(Debug, Serialize, Deserialize)]
pub struct FileChange {
    pub path: PathBuf,
    pub change_type: ChangeType,
    pub staged: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ChangeType {
    Added,
    Modified,
    Deleted,
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
