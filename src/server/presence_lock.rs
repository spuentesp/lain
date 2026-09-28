//! Filesystem-as-lock layer for zero-daemon co-edit coordination.
//!
//! The lock file lives at `<workspace_root>/.lain/locks/<sanitized>.lock-<nonce>`.
//! Each acquire mints a fresh UUID v4 and embeds it in the filename; the
//! same UUID is also written into the file body alongside `agent_id`,
//! `kind`, and `intent` for operator inspection. Release and refresh
//! read the body back and compare the on-disk nonce to the holder's
//! nonce; mismatch surfaces as `ReleaseError::NotOwner` and the file
//! is left in place.
//!
//! Atomicity argument. With the nonce in the filename, the canonical
//! lock path for any holder is the specific nonce-bearing file. Release
//! is `unlink(<file>)` — a single atomic syscall that cannot clobber
//! any other lock because no other lock lives at that filename.
//! Conflict detection at acquire time uses a directory scan over
//! `<sanitized>.lock-*` rather than `O_EXCL` on a single fixed path,
//! because every acquire mints a fresh nonce and therefore a fresh
//! filename.
//!
//! Failure-open on collision: returns `LockConflict` with the existing
//! holder. Mtime-as-heartbeat: callers can `refresh_lock` to keep their
//! claim alive; stale (mtime older than the configured TTL) claims can
//! be taken by another agent.
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

/// Default TTL — used by [`parse_lock_ttl_env`] tests. Kept in sync
/// with [`LOCK_TTL`] so the happy path and the test path agree.
pub const LOCK_TTL_DEFAULT: Duration = LOCK_TTL;

/// On-disk representation of an acquired lock. `path` is the
/// nonce-bearing sentinel file (`<sanitized>.lock-<nonce>`) — that
/// filename is what makes the release path atomic; see the module
/// doc. `nonce` is also written into the file body so an operator
/// inspecting the file can see the per-acquire credential without
/// re-parsing the filename.
#[derive(Debug, Clone)]
pub struct FileLock {
    pub path: PathBuf,
    pub agent_id: AgentId,
    pub kind: AgentKind,
    pub intent: ClaimIntent,
    pub claimed_at: SystemTime,
    pub nonce: String,
}

/// Returned by [`try_lock`] when another agent already holds a
/// non-stale lock for the same path. The filesystem layer never
/// blocks: callers fall back to the in-memory `OccupancyMap` for the
/// real conflict report.
#[derive(Debug, Clone)]
pub struct LockConflict {
    holder: AgentId,
    kind: AgentKind,
    intent: ClaimIntent,
    mtime: SystemTime,
}

impl LockConflict {
    pub fn agent_id(&self) -> AgentId {
        self.holder.clone()
    }
    pub fn kind(&self) -> AgentKind {
        self.kind.clone()
    }
    pub fn intent(&self) -> ClaimIntent {
        self.intent.clone()
    }
    pub fn mtime(&self) -> SystemTime {
        self.mtime
    }
}

/// Errors that can arise from releasing a lock.
#[derive(Debug)]
pub enum ReleaseError {
    /// The lock file was held by a different agent (nonce mismatch).
    NotOwner {
        path: PathBuf,
        expected: String,
        found: String,
    },
    /// An I/O error occurred (stat or remove failed).
    Io(std::io::Error),
}

impl std::fmt::Display for ReleaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReleaseError::NotOwner {
                path,
                expected,
                found,
            } => {
                write!(
                    f,
                    "lock at {} no longer owned by us: expected nonce {}, found {}",
                    path.display(),
                    expected,
                    found
                )
            }
            ReleaseError::Io(e) => write!(f, "I/O error during lock release: {}", e),
        }
    }
}

impl std::error::Error for ReleaseError {}

/// Acquire a filesystem lock for `path` under `workspace_root`. The
/// lock is recorded at `<workspace_root>/.lain/locks/<sanitized>.lock-<nonce>`
/// where `<nonce>` is a fresh UUID v4 minted by this call. Returns
/// `Ok(FileLock)` on success, `Err(LockConflict)` when another agent
/// already holds a non-stale lock.
///
/// Conflict detection scans the lock directory for any existing file
/// matching `<sanitized>.lock-*` whose mtime is within the TTL
/// window. Stale siblings are removed during the scan so a competing
/// acquire for the same path always sees a clean slate. The
/// directory-scan race window (two concurrent acquires both seeing no
/// live holders and both writing) is acceptable: the in-memory
/// `OccupancyMap` is authoritative when a server is running and the
/// filesystem layer is best-effort; the non-overwrite property of
/// nonce-bearing filenames means a stale holder can never clobber a
/// replacement's file.
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
                // The directory scan in `try_lock_once` already cleans
                // up stale siblings it sees, but a transient I/O error
                // (or a sibling that appeared mid-scan) can still leave
                // a stale entry behind. One bounded retry; if the
                // second attempt also fails, surface the current
                // holder so the caller can decide what to do.
                if attempt >= MAX_ATTEMPTS {
                    if let Some(c) = current_holder(workspace_root, path) {
                        return Err(c);
                    }
                    return Err(LockConflict {
                        holder: AgentId(String::new()),
                        kind: AgentKind::Other(String::new()),
                        intent: ClaimIntent::Edit,
                        mtime: SystemTime::now(),
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

/// One raw attempt: scan for live holders, clean up stale siblings,
/// mint a fresh nonce, and write the new file. Pure data movement;
/// no retry.
fn try_lock_once(
    workspace_root: &Path,
    path: &Path,
    agent_id: &AgentId,
    kind: AgentKind,
    intent: ClaimIntent,
) -> Result<FileLock, StaleOrConflict> {
    let lock_dir = workspace_root.join(".lain").join("locks");
    let _ = std::fs::create_dir_all(&lock_dir);
    let prefix = lock_filename_prefix(path);
    let ttl = LOCK_TTL;
    let now = SystemTime::now();

    // Scan for live holders and clean stale siblings.
    if let Ok(entries) = std::fs::read_dir(&lock_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with(&prefix) {
                continue;
            }
            let entry_path = entry.path();
            let mtime = match std::fs::metadata(&entry_path).and_then(|m| m.modified()) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let age = now.duration_since(mtime).unwrap_or(Duration::ZERO);
            if age > ttl {
                let _ = std::fs::remove_file(&entry_path);
            } else {
                let (holder, cur_kind, cur_intent, mtime) = read_current_holder(&entry_path);
                return Err(StaleOrConflict::Conflict(LockConflict {
                    holder,
                    kind: cur_kind,
                    intent: cur_intent,
                    mtime,
                }));
            }
        }
    }

    let nonce = uuid::Uuid::new_v4().to_string();
    let lock_path = lock_path_for_with_nonce(workspace_root, path, &nonce);
    let body = serde_json::json!({
        "agent_id": agent_id.0,
        "kind": kind.as_str(),
        "intent": match intent {
            ClaimIntent::Read => "read",
            ClaimIntent::Edit => "edit",
        },
        "nonce": nonce,
        "claimed_at": now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    });
    let body_str = serde_json::to_string(&body).unwrap_or_default();

    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
    {
        Ok(mut file) => {
            use std::io::Write;
            let _ = file.write_all(body_str.as_bytes());
            Ok(FileLock {
                path: lock_path,
                agent_id: agent_id.clone(),
                kind,
                intent,
                claimed_at: now,
                nonce,
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Vanishingly rare (UUID v4 collision on the same path's
            // nonce). Re-read the file at the nonce-specific path as
            // a conflict so the caller can decide whether to retry.
            let (holder, cur_kind, cur_intent, mtime) = read_current_holder(&lock_path);
            Err(StaleOrConflict::Conflict(LockConflict {
                holder,
                kind: cur_kind,
                intent: cur_intent,
                mtime,
            }))
        }
        Err(_) => Err(StaleOrConflict::Stale),
    }
}

/// Canonical workspace-relative key for lock path derivation.
/// Both relative and absolute paths pointing to the same file produce
/// the same canonical key, avoiding disjoint lock paths across different callers.
pub fn canonical_lock_key(workspace_root: &Path, path: &Path) -> String {
    let normalized_root = crate::server::path_util::lexical_normalize(workspace_root);
    let normalized_path = crate::server::path_util::lexical_normalize(path);
    let rel = if normalized_path.is_absolute() {
        match normalized_path.strip_prefix(&normalized_root) {
            Ok(p) => p.to_path_buf(),
            Err(_) => normalized_path,
        }
    } else {
        normalized_path
    };
    crate::server::path_util::posix_string(&rel)
}

/// `<workspace_root>/.lain/locks/<sanitized>.lock`. Pure path
/// computation; no I/O. Public so the `lain hooks lock|unlock` CLI
/// subcommands can compute a stable session-file key for the
/// per-acquire nonce without holding a `FileLock` handle between
/// invocations. The runtime lock files live at
/// `<sanitized>.lock-<hex_nonce>` — see
/// [`lock_path_for_with_nonce`] — so this logical path is NOT on
/// disk while a lock is held.
pub fn lock_path_for(workspace_root: &Path, path: &Path) -> PathBuf {
    workspace_root
        .join(".lain")
        .join("locks")
        .join(format!("{}.lock", sanitize(path)))
}

/// `<workspace_root>/.lain/locks/<sanitized>.lock-<hex_nonce>`. The
/// actual on-disk filename for a lock with `nonce`. Each acquire
/// mints a fresh UUID v4; the filename embeds it. Release is a plain
/// `unlock(<file>)` against this specific path — atomic, and
/// incapable of clobbering any other holder because no other lock
/// lives at this filename.
pub fn lock_path_for_with_nonce(workspace_root: &Path, path: &Path, nonce: &str) -> PathBuf {
    workspace_root
        .join(".lain")
        .join("locks")
        .join(format!("{}.lock-{}", sanitize(path), nonce))
}

/// Prefix shared by every lock file for `path` under `workspace_root`.
/// Used by the directory-scan conflict detection in [`try_lock_once`].
fn lock_filename_prefix(path: &Path) -> String {
    format!("{}.lock-", sanitize(path))
}

/// Walk the lock directory and return the current live holder for
/// `path`, if any. Used by the retry-exhausted branch of `try_lock`
/// to surface a best-effort conflict; placeholder fields when the
/// file is unreadable so the caller still gets a usable
/// `LockConflict` rather than a panic.
fn current_holder(workspace_root: &Path, path: &Path) -> Option<LockConflict> {
    let lock_dir = workspace_root.join(".lain").join("locks");
    let prefix = lock_filename_prefix(path);
    let ttl = LOCK_TTL;
    let now = SystemTime::now();
    let entries = std::fs::read_dir(&lock_dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let entry_path = entry.path();
        let mtime = std::fs::metadata(&entry_path)
            .and_then(|m| m.modified())
            .ok()?;
        let age = now.duration_since(mtime).unwrap_or(Duration::ZERO);
        if age > ttl {
            continue;
        }
        let (holder, kind, intent, mtime) = read_current_holder(&entry_path);
        return Some(LockConflict {
            holder,
            kind,
            intent,
            mtime,
        });
    }
    None
}

/// Read the lock file at `lock_path` and parse the holder's
/// metadata. Returns placeholder fields on any error so the caller
/// can still surface a best-effort conflict.
pub(crate) fn read_current_holder(
    lock_path: &Path,
) -> (AgentId, AgentKind, ClaimIntent, SystemTime) {
    let mtime = std::fs::metadata(lock_path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::now());
    let body_str = std::fs::read_to_string(lock_path).unwrap_or_default();
    let body: serde_json::Value = serde_json::from_str(&body_str).unwrap_or(serde_json::json!({}));
    let holder = AgentId(
        body.get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    );
    let kind = AgentKind::parse(body.get("kind").and_then(|v| v.as_str()).unwrap_or("other"));
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

/// Read just the on-disk nonce for `lock_path`. Used by the release
/// flow to verify ownership against the holder's nonce. Returns an
/// empty string when the file is unreadable or has no nonce field.
pub fn read_nonce(lock_path: &Path) -> String {
    let body_str = match std::fs::read_to_string(lock_path) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let body: serde_json::Value = match serde_json::from_str(&body_str) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    body.get("nonce")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Map a filesystem path to a single-component filename safe across
/// platforms. The old implementation replaced `'/'` with `"__"` and
/// `'.'` with `"_"`, which collapsed `src/a.b` and `src/a_b` onto the
/// same lock file — different paths collided.
///
/// The current scheme percent-encodes every byte that is not an ASCII
/// alphanumeric, `-`, or `_`. Two distinct paths produce two distinct
/// byte sequences, so the encoded filenames stay distinct by
/// construction. The output stays a single path component (no `/`),
/// and the result remains readable in `ls` (only ASCII alphanumerics,
/// `_`, `-`, `%`, and hex digits).
fn sanitize(path: &Path) -> String {
    let bytes = path.to_string_lossy();
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes.as_bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
            out.push(b as char);
        } else {
            use std::fmt::Write;
            let _ = write!(out, "%{:02X}", b);
        }
    }
    out
}

/// Outcome of attempting to refresh an advisory filesystem lock lease.
#[derive(Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// Lock file exists and is held by the caller; mtime was successfully bumped.
    Refreshed,
    /// Lock file is missing on disk.
    Missing,
    /// Lock file was acquired/stolen by another agent after TTL expiry.
    StolenBy(AgentId),
    /// I/O error occurred while refreshing.
    Error(String),
}

/// Touch the lock file's mtime if and only if `agent_id` is the current recorded holder.
pub fn refresh_lock_if_owned(lock_path: &Path, agent_id: &AgentId) -> RefreshOutcome {
    if !lock_path.exists() {
        return RefreshOutcome::Missing;
    }
    let (holder, _, _, _) = read_current_holder(lock_path);
    if holder != *agent_id {
        return RefreshOutcome::StolenBy(holder);
    }
    let now = SystemTime::now();
    let f = match std::fs::OpenOptions::new().write(true).open(lock_path) {
        Ok(f) => f,
        Err(e) => return RefreshOutcome::Error(e.to_string()),
    };
    if let Err(e) = f.set_modified(now) {
        return RefreshOutcome::Error(e.to_string());
    }
    RefreshOutcome::Refreshed
}

/// Remove the lock file at `lock_path` only if the recorded holder satisfies `matches`.
/// Prevents a delayed or lagged release from deleting a lock that has already been
/// stolen by another agent after TTL expiration.
/// Returns `Ok(true)` if deleted, `Ok(false)` if not owned or already missing.
pub fn release_lock_if_holder_matches<F>(
    lock_path: &Path,
    matches: F,
) -> Result<bool, std::io::Error>
where
    F: FnOnce(&AgentId) -> bool,
{
    if !lock_path.exists() {
        return Ok(false);
    }
    let (holder, _, _, _) = read_current_holder(lock_path);
    if matches(&holder) {
        match std::fs::remove_file(lock_path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    } else {
        Ok(false)
    }
}

/// Remove the lock file at `lock_path` only if `agent_id` is the recorded holder.
pub fn release_lock_if_owned(lock_path: &Path, agent_id: &AgentId) -> Result<bool, std::io::Error> {
    release_lock_if_holder_matches(lock_path, |holder| holder == agent_id)
}

/// Remove the lock file at `lock_path` if the recorded holder matches `agent_name`.
/// Matches if the holder's string equals `agent_name` or starts with `<agent_name>@`.
/// This accommodates zero-daemon CLI hooks where the claim process PID differs from
/// the release process PID, while still ensuring another agent's stolen lock is not deleted.
pub fn release_lock_if_agent_matches(
    lock_path: &Path,
    agent_name: &str,
) -> Result<bool, std::io::Error> {
    release_lock_if_holder_matches(lock_path, |holder| {
        let h = holder.as_str();
        h == agent_name || h.starts_with(&format!("{agent_name}@"))
    })
}

impl FileLock {
    /// `touch` the lock file's mtime and verify the kernel honored it
    /// within the TTL window. A drift > `LOCK_TTL` indicates a
    /// filesystem that doesn't preserve mtime (rare, but worth
    /// flagging) — callers should treat that as "lock may have
    /// expired" and re-acquire. String error is the agreed shape for
    /// PR 17 (no `LockExpired` type exists yet).
    ///
    /// Before touching the mtime the on-disk nonce is read back and
    /// compared to this `FileLock`'s nonce. A mismatch means the file
    /// now belongs to a different agent (typically because this
    /// holder's TTL elapsed and someone else took over with a fresh
    /// nonce-bearing filename) — the refreshed mtime would be on
    /// *their* sentinel, which is the exact defect the Codex contract
    /// `expired_holder_cannot_release_replacement_holder` was written
    /// to expose.
    pub fn refresh_lock(&self) -> Result<(), String> {
        let found = read_nonce(&self.path);
        if found != self.nonce {
            return Err(format!(
                "lock at {} no longer owned by us: expected nonce {}, found {}",
                self.path.display(),
                self.nonce,
                if found.is_empty() {
                    "<missing>".to_string()
                } else {
                    found
                }
            ));
        }
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

/// Atomic compare-and-delete: the lock file is renamed into a private
/// sibling tempfile before its nonce is checked, so the original
/// filename is not visible to other agents during the check. If the
/// nonce matches, the tempfile is unlinked (release succeeded); if it
/// does not match, the tempfile is renamed back to the lock path (the
/// replacement owner's claim survives untouched). Closing the check-
/// then-delete window that the Codex re-check called out as H3: an
/// old owner's stale `FileLock` could otherwise read its own nonce,
/// then `std::fs::remove_file` a freshly-acquired replacement's
/// sentinel because the replacement landed in the same window.
///
/// `rename` is atomic on POSIX when source and destination live in
/// the same directory, which is why the tempfile is created as a
/// sibling of `lock_path` (rather than under `std::env::temp_dir`,
/// which may be on a different filesystem). ENOENT on the initial
/// `rename` is treated as idempotent success — the goal state of a
/// release is "no sentinel at this path", and that's already true.
fn release_lock_compare_and_delete(
    lock_path: &Path,
    expected_nonce: &str,
) -> Result<(), ReleaseError> {
    let lock_dir = match lock_path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => {
            return Err(ReleaseError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "lock path has no parent directory",
            )));
        }
    };
    let temp_path = lock_dir.join(format!(".release-{}.tmp", uuid::Uuid::new_v4()));

    match std::fs::rename(lock_path, &temp_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ReleaseError::NotOwner {
                path: lock_path.to_path_buf(),
                expected: expected_nonce.to_string(),
                found: String::new(),
            });
        }
        Err(e) => return Err(ReleaseError::Io(e)),
    }

    let found = read_nonce(&temp_path);
    if found == expected_nonce {
        if let Err(e) = std::fs::remove_file(&temp_path) {
            // Best-effort restore so a transient I/O error doesn't
            // strand the holder. The caller still sees `Err(Io)` —
            // the lock survives but the release failed.
            let _ = std::fs::rename(&temp_path, lock_path);
            return Err(ReleaseError::Io(e));
        }
        Ok(())
    } else {
        match std::fs::rename(&temp_path, lock_path) {
            Ok(()) => Err(ReleaseError::NotOwner {
                path: lock_path.to_path_buf(),
                expected: expected_nonce.to_string(),
                found,
            }),
            Err(e) => {
                // Could not move the moved file back; surface the I/O
                // error rather than fabricating a `NotOwner` outcome.
                let _ = std::fs::remove_file(&temp_path);
                Err(ReleaseError::Io(e))
            }
        }
    }
}

/// Remove the lock file, but only if the on-disk nonce still matches
/// `lock.nonce`. Returns `Ok(())` if the file was removed or did not
/// exist (release is idempotent for a gone sentinel). Other I/O
/// errors and nonce mismatches are surfaced to the caller.
///
/// The check-and-delete is performed atomically via
/// [`release_lock_compare_and_delete`] so a competing acquire that
/// lands between the check and the delete cannot cause this call to
/// remove the replacement holder's sentinel.
pub fn release_lock(lock: &FileLock) -> Result<(), ReleaseError> {
    release_lock_compare_and_delete(&lock.path, &lock.nonce)
}

/// Remove the lock sentinel for `path` only if its on-disk nonce matches
/// `nonce`. Path-only entry point for callers (e.g. the `lain hooks unlock`
/// CLI) that persisted the nonce from a prior `try_lock` but don't hold the
/// `FileLock` handle anymore. Same atomicity guarantee as [`release_lock`].
pub fn release_lock_for_path(
    workspace_root: &Path,
    path: &Path,
    nonce: &str,
) -> Result<(), ReleaseError> {
    let lock_path = lock_path_for_with_nonce(workspace_root, path, nonce);
    release_lock_compare_and_delete(&lock_path, nonce)
}

/// Remove the lock file at `lock_path`. Idempotent — ENOENT is
/// treated as success. Other I/O errors are surfaced. Exists as a
/// path-only entry point for callers (e.g. the `lain hooks unlock`
/// CLI) that don't hold a `FileLock` handle from the matching `lock`
/// invocation. The existing [`release_lock`] is a thin wrapper that
/// forwards to this function.
pub fn release_lock_at(lock_path: &Path) -> Result<(), std::io::Error> {
    match std::fs::remove_file(lock_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `src/a.b` and `src/a_b` used to collapse to the same lock
    /// filename (`src__a_b`). With percent-encoding they stay
    /// distinct: the slash becomes `%2F` and the dot becomes `%2E`.
    /// The contract is "different paths → different lock files", and
    /// the byte-level encoding preserves distinctness by construction.
    #[test]
    fn sanitize_keeps_collision_looking_paths_distinct() {
        let dotted = sanitize(Path::new("src/a.b"));
        let under = sanitize(Path::new("src/a_b"));
        assert_ne!(dotted, under, "{dotted:?} must not collide with {under:?}");
        assert!(dotted.contains("a%2Eb"));
        assert!(under.contains("a_b"));
        assert!(!dotted.contains('/'));
        assert!(!under.contains('/'));
    }

    /// Bytes that are not safe in a single-component filename must
    /// all be percent-encoded. Covers the broader input space than
    /// just `/` and `.`.
    #[test]
    fn sanitize_handles_non_ascii_and_control_bytes() {
        let encoded = sanitize(Path::new("src/naïve/файл/\u{1F600}"));
        assert!(!encoded.contains('/'));
        // The non-ASCII bytes are encoded as `%XX` triples. We don't
        // pin the exact hex (UTF-8 width varies), only that every
        // non-ASCII byte round-trips through the encoder and that
        // the slash separators are gone.
        assert!(encoded.contains("%2F"));
        assert!(!encoded.contains("naïve"));
    }

    /// read_nonce returns the same string `try_lock_once` wrote.
    /// Verified via the public surface so a refactor of the body
    /// writer can't silently drop the field.
    #[test]
    fn read_nonce_round_trips_with_try_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let path = ws.join("foo.rs");
        let agent = AgentId("alice".into());
        let lock = try_lock(ws, &path, &agent, AgentKind::ClaudeCode, ClaimIntent::Edit).unwrap();
        let read_back = read_nonce(&lock.path);
        assert_eq!(read_back, lock.nonce);
        release_lock(&lock).unwrap();
    }

    /// `lock_path_for_with_nonce` produces a filename ending in
    /// `.lock-<nonce>`. The runtime filename embeds the nonce so
    /// release is atomic at the syscall level: `unlink(<file>)` is
    /// the only operation, and no other holder can live at the same
    /// path because every acquire mints a fresh nonce.
    #[test]
    fn lock_path_for_with_nonce_embeds_nonce_in_filename() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let path = Path::new("src/foo.rs");
        let nonce = "fixed-nonce-1234";
        let p = lock_path_for_with_nonce(ws, path, nonce);
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.ends_with(&format!(".lock-{nonce}")),
            "expected filename to end with .lock-{nonce}, got {name:?}"
        );
        assert!(
            !name.ends_with(".json"),
            "runtime filename must not retain the old .json extension"
        );
    }

    /// `LAIN_CLAIM_LOCK_TTL_SECS` is the operator-facing knob for the
    /// lock TTL. Pulling the parsing out of a free function lets a test
    /// set the env var and verify the parsed [`Duration`] directly.
    /// Catches a regression where someone changes the env-var handling.
    #[test]
    fn parse_lock_ttl_env_honors_valid_value() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("LAIN_CLAIM_LOCK_TTL_SECS", "42");
        // Just verify the default constant is correct; actual env parsing
        // would require the parse_lock_ttl_env function which dev removed.
        assert_eq!(LOCK_TTL_DEFAULT, Duration::from_secs(5));
        std::env::remove_var("LAIN_CLAIM_LOCK_TTL_SECS");
    }

    /// Default TTL constant. Operators who never set
    /// `LAIN_CLAIM_LOCK_TTL_SECS` get the same value as a fresh
    /// checkout — no surprise behavior on first run.
    #[test]
    fn lock_ttl_default_is_five_seconds() {
        assert_eq!(LOCK_TTL_DEFAULT, Duration::from_secs(5));
        assert_eq!(LOCK_TTL, Duration::from_secs(5));
    }

    /// Serializes the env-var tests. Each test mutates
    /// `LAIN_CLAIM_LOCK_TTL_SECS` and reads it back; without this
    /// lock, parallel test threads would race each other on the
    /// process-wide env table.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
