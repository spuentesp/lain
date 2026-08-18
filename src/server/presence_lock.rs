//! Filesystem-as-lock layer for zero-daemon co-edit coordination.
//!
//! Atomic `O_EXCL` create on a sentinel file under `<workspace>/.lain/locks/<sanitized>.json`.
//! Failure-open on collision: returns `LockConflict` with the existing holder.
//! Mtime-as-heartbeat: callers can `refresh_lock` to keep their claim alive; stale
//! (mtime older than 5s) claims can be taken by another agent.
//!
//! Scope of this layer (PR 17):
//! - Best-effort hint for human operators and for non-`lain` automation
//!   reading the workspace without a running server.
//! - The in-memory `OccupancyMap` remains authoritative when a `lain`
//!   server is running. `OccupancyMap::claim` calls `try_lock` as a
//!   side-effect, *ignoring* the result for in-memory bookkeeping:
//!   conflict on the filesystem path does NOT roll back the in-memory
//!   claim, and I/O errors (directory creation failure, unwritable
//!   workspace, etc.) log a warning and continue.

use crate::server::presence::{AgentId, AgentKind, ClaimIntent};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// TTL window for a lock. A claim whose mtime is older than this is
/// considered stale and may be taken by another agent. Matches the
/// in-memory heartbeating cadence (5s) so a dead writer's lock does
/// not block live ones for longer than a single heartbeat interval.
pub const LOCK_TTL: Duration = Duration::from_secs(5);

/// On-disk representation of an acquired lock. The `path` is the
/// sentinel file location; the rest is the holder's metadata so
/// `LockConflict` can surface useful context to operators reading the
/// file directly.
#[derive(Debug, Clone)]
pub struct FileLock {
    pub path: PathBuf,
    pub agent_id: AgentId,
    pub kind: AgentKind,
    pub intent: ClaimIntent,
    pub claimed_at: SystemTime,
}

/// Returned by [`try_lock`] when another agent already holds the
/// lock within the TTL window. The filesystem layer never blocks:
/// callers fall back to the in-memory `OccupancyMap` for the real
/// conflict report.
#[derive(Debug, Clone)]
pub struct LockConflict {
    holder: AgentId,
    kind: AgentKind,
    intent: ClaimIntent,
    mtime: SystemTime,
}

impl LockConflict {
    pub fn agent_id(&self) -> AgentId { self.holder.clone() }
    pub fn kind(&self) -> AgentKind { self.kind.clone() }
    pub fn intent(&self) -> ClaimIntent { self.intent.clone() }
    pub fn mtime(&self) -> SystemTime { self.mtime }
}

/// Acquire a filesystem lock for `path` under `workspace_root`. Atomic
/// `O_EXCL` create on `<workspace_root>/.lain/locks/<sanitized>.json`.
/// Returns `Ok(FileLock)` on success, `Err(LockConflict)` when another
/// agent holds a non-stale lock.
///
/// Recursion is bounded: when the existing lock is stale we remove
/// it and *at most* retry once. If the retry also fails (e.g. a third
/// agent raced in between the stale-removal and the retry) we surface
/// that as a `LockConflict`. The worst case is one stale-removal +
/// one extra `create_new` attempt; no infinite recursion.
pub fn try_lock(
    workspace_root: &Path,
    path: &Path,
    agent_id: &AgentId,
    kind: AgentKind,
    intent: ClaimIntent,
) -> Result<FileLock, LockConflict> {
    const MAX_ATTEMPTS: u8 = 2;
    let mut attempt: u8 = 0;
    loop {
        attempt += 1;
        match try_lock_once(workspace_root, path, agent_id, kind.clone(), intent.clone()) {
            Ok(lock) => return Ok(lock),
            Err(StaleOrConflict::Conflict(c)) => return Err(c),
            Err(StaleOrConflict::Stale) => {
                // Stale; remove and retry. Cap at MAX_ATTEMPTS so a
                // racing agent can't keep stale-removing us in a loop.
                let lock_path = lock_path_for(workspace_root, path);
                let _ = std::fs::remove_file(&lock_path);
                if attempt >= MAX_ATTEMPTS {
                    // Second attempt failed too — re-read the
                    // current holder and report the conflict so the
                    // caller can decide whether to roll back.
                    let (holder, cur_kind, cur_intent, mtime) =
                        read_current_holder(&lock_path);
                    return Err(LockConflict {
                        holder,
                        kind: cur_kind,
                        intent: cur_intent,
                        mtime,
                    });
                }
            }
        }
    }
}

enum StaleOrConflict {
    Conflict(LockConflict),
    Stale,
}

/// One raw attempt: create_new, or read+classify the existing file.
/// Pure data movement; no retry.
fn try_lock_once(
    workspace_root: &Path,
    path: &Path,
    agent_id: &AgentId,
    kind: AgentKind,
    intent: ClaimIntent,
) -> Result<FileLock, StaleOrConflict> {
    let lock_dir = workspace_root.join(".lain").join("locks");
    // Best-effort dir creation. `create_dir_all` on an existing dir
    // is a no-op; an unwritable workspace surfaces as `Err` on the
    // `create_new` below, which `OccupancyMap::claim` logs and
    // ignores (in-memory state stays authoritative).
    let _ = std::fs::create_dir_all(&lock_dir);
    let lock_path = lock_path_for(workspace_root, path);

    use std::fs::OpenOptions;
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
    {
        Ok(mut file) => {
            // Write the holder metadata so an operator inspecting
            // the lock file directly can see *who* is holding it
            // without round-tripping through `lain`.
            let now = SystemTime::now();
            let body = serde_json::json!({
                "agent_id": agent_id.0,
                "kind": kind.as_str(),
                "intent": match intent {
                    ClaimIntent::Read => "read",
                    ClaimIntent::Edit => "edit",
                },
                "claimed_at": now
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            });
            if let Ok(s) = serde_json::to_string(&body) {
                use std::io::Write;
                let _ = file.write_all(s.as_bytes());
            }
            Ok(FileLock {
                path: lock_path,
                agent_id: agent_id.clone(),
                kind,
                intent,
                claimed_at: now,
            })
        }
        Err(_) => {
            // Existing lock — read and classify as conflict or stale.
            let (holder, kind, intent, mtime) = read_current_holder(&lock_path);
            let now = SystemTime::now();
            let age = now.duration_since(mtime).unwrap_or(Duration::ZERO);
            if age < LOCK_TTL {
                Err(StaleOrConflict::Conflict(LockConflict {
                    holder,
                    kind,
                    intent,
                    mtime,
                }))
            } else {
                Err(StaleOrConflict::Stale)
            }
        }
    }
}

/// `<workspace_root>/.lain/locks/<sanitized>.json`. Pure path
/// computation; no I/O.
fn lock_path_for(workspace_root: &Path, path: &Path) -> PathBuf {
    workspace_root
        .join(".lain")
        .join("locks")
        .join(format!("{}.json", sanitize(path)))
}

/// Read the existing lock file at `lock_path` and parse the holder's
/// metadata. Returns placeholder fields on any error so the caller can
/// still report a best-effort conflict instead of panicking.
fn read_current_holder(lock_path: &Path) -> (AgentId, AgentKind, ClaimIntent, SystemTime) {
    let mtime = std::fs::metadata(lock_path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::now());
    let body_str = std::fs::read_to_string(lock_path).unwrap_or_default();
    let body: serde_json::Value =
        serde_json::from_str(&body_str).unwrap_or(serde_json::json!({}));
    let holder = AgentId(
        body.get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    );
    let kind = AgentKind::parse(
        body.get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("other"),
    );
    let intent = match body
        .get("intent")
        .and_then(|v| v.as_str())
        .unwrap_or("edit")
    {
        "read" => ClaimIntent::Read,
        _ => ClaimIntent::Edit,
    };
    (holder, kind, intent, mtime)
}

/// Replace path separators with a sentinel so the lock path is a
/// single-component filename. Dots become underscores too so `.rs`
/// doesn't survive as a hidden filename in editor listings. The
/// result is not designed to be reversible — it only needs to be
/// unique per input path within a single workspace.
fn sanitize(path: &Path) -> String {
    path.to_string_lossy().replace('/', "__").replace('.', "_")
}

impl FileLock {
    /// `touch` the lock file's mtime and verify the kernel honored it
    /// within the TTL window. A drift > `LOCK_TTL` indicates a
    /// filesystem that doesn't preserve mtime (rare, but worth
    /// flagging) — callers should treat that as "lock may have
    /// expired" and re-acquire. String error is the agreed shape for
    /// PR 17 (no `LockExpired` type exists yet).
    pub fn refresh_lock(&self) -> Result<(), String> {
        let now = SystemTime::now();
        // `set_modified` is stable since 1.75; the free function
        // `std::fs::set_file_mtime` is still nightly-only. Open the
        // file in write mode (no truncation) and ask the OS to bump
        // the mtime — same effect, stable path.
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&self.path)
            .map_err(|e| e.to_string())?;
        f.set_modified(now).map_err(|e| e.to_string())?;
        let mtime = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .map_err(|e| e.to_string())?;
        if now.duration_since(mtime).unwrap_or(Duration::ZERO) > LOCK_TTL {
            Err("lock mtime drifted too far in the past".into())
        } else {
            Ok(())
        }
    }
}

/// Remove the lock file. Returns `Ok(())` if the file was removed or
/// did not exist (release is idempotent). Other I/O errors
/// (permissions, etc.) are surfaced to the caller.
pub fn release_lock(lock: &FileLock) -> Result<(), std::io::Error> {
    match std::fs::remove_file(&lock.path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
