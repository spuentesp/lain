//! Audit log — append-only JSONL record of every edit that lands on
//! disk under the server's state directory.
//!
//! The log is the durable, forensic counterpart to the volatile SSE
//! `edit_landed` event (PR 2 Task 2.4). Every successful write path
//! (manual edit resolution, federation `write` MCP tool, etc.) calls
//! `append_edit_event` once the bytes hit the filesystem; the entry
//! records the agent, the path, the claim set, the racers, the plan
//! revision observed at write time, and the landed revision counter
//! from the overlay broadcast.
//!
//! Storage model: one `audit.jsonl` file per state directory, plus
//! a single rotated sibling `audit.jsonl.1` that holds the previous
//! window. Rotation triggers when the live file is at-or-above
//! `AUDIT_LOG_MAX_BYTES` (50 MB) at the moment a new event is about
//! to be appended — the live file is renamed over `audit.jsonl.1`
//! (overwriting any prior rotation) and a fresh `audit.jsonl` is
//! created for the new event. This keeps the worst-case on-disk
//! footprint at ~2× the cap and the read path to exactly two files
//! (`audit.jsonl` + `audit.jsonl.1`), which `read_audit_log` walks
//! in order.
//!
//! Concurrency: this module assumes a single writer (the owning
//! server process). Multiple writers would need an external lock —
//! the rotation rename is a single atomic syscall on POSIX, so the
//! only race is between two appenders racing the rotation check
//! and the append itself, which is not a concern in practice.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::server::overlay::stream::RevisionId;
use crate::server::presence::{AgentId, Claim, ConflictEntry};

/// Filename of the live audit log under the state directory.
pub const AUDIT_LOG_FILENAME: &str = "audit.jsonl";
/// Filename of the rotated audit log sibling. At most one rotation
/// window is kept; a second rotation overwrites this file.
pub const AUDIT_LOG_ROTATED: &str = "audit.jsonl.1";
/// Rotation trigger — when the live file is at-or-above this size
/// (in bytes), the next append rotates it to `AUDIT_LOG_ROTATED`.
/// Production cap is 50 MB; tests exercise the exact-at-cap edge.
pub const AUDIT_LOG_MAX_BYTES: u64 = 50 * 1024 * 1024;

/// Per-write context the caller can attach to a write path. Carries
/// the snapshot the writer reasoned over (so an offline auditor can
/// reconstruct *why* the writer believed the write was safe), the
/// other agents holding conflicting claims at write time, and the
/// plan revision the writer was synchronized with.
///
/// Currently surfaced through the audit pipeline; reserved here as
/// the canonical struct so the wire format and the audit JSON line
/// share the same shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteContext {
    /// Claim snapshot the writer resolved against — *before* the
    /// write was applied. Empty for write paths that don't consult
    /// presence (e.g. raw fs ops).
    pub claim_snapshot: Vec<Claim>,
    /// Other agents that were known to be editing concurrent
    /// symbols at write time (post-resolution view).
    pub concurrent_editors_at_write: Vec<AgentId>,
    /// Plan revision the writer was synchronized with at the moment
    /// the write was issued.
    pub as_of_revision: RevisionId,
}

/// One audit log entry. The seven public fields are the contract —
/// downstream consumers (PR 2.5 `get_audit_log` MCP tool, audit
/// offset persistence, future forensic tools) all key off this
/// exact shape. The struct serializes as a single JSON object per
/// line, terminated by `\n`, in `audit.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Wall-clock time the event was recorded, as a UNIX-epoch
    /// fractional second count. `f64` for the same reason the rest
    /// of the server uses fractional seconds — sub-second ordering
    /// is occasionally useful when two writes land in the same
    /// millisecond, and serde_json's `f64` round-trip is exact for
    /// the values we generate here.
    pub ts_unix: f64,
    /// The agent that issued the write.
    pub agent_id: AgentId,
    /// The file path the write targeted, relative to the workspace
    /// root (or absolute if the writer couldn't normalize it).
    pub path: PathBuf,
    /// The claim set the writer had at the moment of the write —
    /// i.e. the post-resolution claims the writer believed itself
    /// to hold on `path`.
    pub claim_set: Vec<Claim>,
    /// Other agents that held *conflicting* claims at write time,
    /// reported back to the writer by presence resolution. Empty
    /// when the write was uncontested.
    pub racers: Vec<ConflictEntry>,
    /// The plan revision the writer had synchronized to at the
    /// moment of the write. `None` for legacy writers or writers
    /// that don't track plan revision yet.
    pub plan_revision: Option<RevisionId>,
    /// The overlay revision counter that observed the write's
    /// effects land. Surfaced verbatim from the broadcast channel
    /// so audit and SSE consumers reference the same monotone.
    pub landed_revision: RevisionId,
}

/// Append a single audit event to `audit.jsonl` under `state_dir`.
///
/// If the live file already exists and is at-or-above
/// `AUDIT_LOG_MAX_BYTES`, the live file is renamed over the rotated
/// sibling (`audit.jsonl.1`), clobbering any prior rotation, and a
/// fresh `audit.jsonl` is created for the new event. Otherwise the
/// event is appended to the existing live file (or a new one is
/// created on first call).
///
/// The event is serialized as a single line of JSON followed by a
/// `\n`. Lines are not pretty-printed; consumers that want a
/// human-readable view should re-format downstream.
///
/// # Errors
///
/// Returns the underlying `std::io::Error` for filesystem failures
/// (rotation rename, open, write, flush). A `serde_json` error
/// during event serialization is converted to an `io::Error` of
/// `ErrorKind::Other` so the public surface stays in `io::Result`.
pub fn append_edit_event(state_dir: &Path, event: &AuditEvent) -> std::io::Result<()> {
    let path = state_dir.join(AUDIT_LOG_FILENAME);

    // Rotation: at-or-above the cap triggers a rename. We do this
    // *before* opening the live file for append so the post-rotate
    // open creates a fresh file rather than growing the rotated
    // one. Removing the rotated sibling first keeps the rename
    // atomic on POSIX (rename(2) refuses to clobber an existing
    // destination on some filesystems) — the `let _ =` swallows
    // the not-found error which is the common case on first
    // rotation.
    if path.exists() {
        let size = std::fs::metadata(&path)?.len();
        if size >= AUDIT_LOG_MAX_BYTES {
            let rotated = state_dir.join(AUDIT_LOG_ROTATED);
            let _ = std::fs::remove_file(&rotated);
            std::fs::rename(&path, &rotated)?;
        }
    }

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let line = serde_json::to_string(event)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    writeln!(f, "{line}")?;
    Ok(())
}

/// Read audit events from the state directory, optionally filtering
/// out events older than `since_unix`. Reads both `audit.jsonl` and
/// `audit.jsonl.1` (in that order) so callers see the full history
/// within the rotation window. Malformed lines are silently skipped
/// — a single corrupt line must not block the entire read path.
///
/// Deduplication is not needed: rotation renames the live file
/// atomically over the rotated sibling, so any event present in
/// `audit.jsonl.1` is by construction not present in `audit.jsonl`.
/// The two reads therefore see disjoint event sets.
///
/// # Errors
///
/// Returns the underlying `std::io::Error` for filesystem failures
/// (open, read). The function tolerates a missing `audit.jsonl` (no
/// events yet) and a missing `audit.jsonl.1` (no rotation yet).
pub fn read_audit_log(
    state_dir: &Path,
    since_unix: Option<f64>,
) -> std::io::Result<Vec<AuditEvent>> {
    let mut out = Vec::new();
    for name in [AUDIT_LOG_FILENAME, AUDIT_LOG_ROTATED] {
        let p = state_dir.join(name);
        if !p.exists() {
            continue;
        }
        let f = std::fs::File::open(&p)?;
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            if line.is_empty() {
                continue;
            }
            // Malformed lines are skipped, not fatal — see fn docs.
            if let Ok(ev) = serde_json::from_str::<AuditEvent>(&line) {
                if let Some(since) = since_unix {
                    if ev.ts_unix < since {
                        continue;
                    }
                }
                out.push(ev);
            }
        }
    }
    Ok(out)
}

/// Size in bytes of the live `audit.jsonl` under `state_dir`, i.e.
/// the byte offset at which the next `append_edit_event` call will
/// start writing. Returns `0` if the file is missing (fresh server)
/// or unreadable (treat missing and unreadable the same: a brand-new
/// append will create the file fresh). The rotated sibling is
/// ignored — by the time the next append fires, rotation will have
/// either already happened (sibling is fresh) or be about to (the
/// next append decides based on the live file's size, which is what
/// we report here).
///
/// This is the source of truth that `PersistedState::audit_offset_bytes`
/// is persisted from at save time (Task 2.6). The OS's append-only
/// positioning means the audit module never *needs* the persisted
/// value to seek — `O_APPEND` already lands the write at the current
/// end of file — but the field is still useful as a "last known good"
/// marker for diagnostics and for `get_audit_log` consumers.
pub fn current_offset_bytes(state_dir: &Path) -> u64 {
    let path = state_dir.join(AUDIT_LOG_FILENAME);
    match std::fs::metadata(&path) {
        Ok(m) => m.len(),
        // Missing or unreadable → "no audit data yet" → offset 0.
        Err(_) => 0,
    }
}

/// True when the live `audit.jsonl` exists and is openable under
/// `state_dir`. The loader (Task 2.6) uses this to decide whether
/// to stamp a fresh `audit_reset_at_unix` on the persisted state
/// file: if the audit log is gone, the next save would otherwise
/// silently re-emit a stale offset that doesn't correspond to any
/// real audit data, so the loader marks the gap and resets the
/// offset to 0. The rotated sibling is not consulted — rotation is
/// the live file's problem, and a missing live file is already
/// enough to flag a reset.
pub fn audit_log_present_and_readable(state_dir: &Path) -> bool {
    let path = state_dir.join(AUDIT_LOG_FILENAME);
    std::fs::File::open(&path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_event(ts: f64, agent: &str) -> AuditEvent {
        AuditEvent {
            ts_unix: ts,
            agent_id: AgentId(agent.to_string()),
            path: PathBuf::from("/x.rs"),
            claim_set: vec![],
            racers: vec![],
            plan_revision: None,
            landed_revision: 1,
        }
    }

    /// One append produces one JSONL line containing the `ts_unix`
    /// field — sanity check that the wire format round-trips.
    #[test]
    fn append_writes_one_jsonl_line() {
        let tmp = tempdir().unwrap();
        let event = sample_event(1.0, "a-1");
        append_edit_event(tmp.path(), &event).unwrap();

        let log = tmp.path().join(AUDIT_LOG_FILENAME);
        let body = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 1, "expected exactly one line, got: {body}");

        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert!(parsed.get("ts_unix").is_some(), "ts_unix missing: {parsed}");
        // The other contract fields are all there too.
        for k in ["ts_unix", "agent_id", "path", "claim_set", "racers", "plan_revision", "landed_revision"] {
            assert!(parsed.get(k).is_some(), "field {k} missing: {parsed}");
        }
    }

    /// Sentinel == cap rotates on the very next append. Pre-fill
    /// `audit.jsonl` to exactly `AUDIT_LOG_MAX_BYTES`, seed the
    /// rotated file with known content, append one real event, and
    /// verify the live file moved over the rotated sibling (which
    /// now has sentinel bytes) while a fresh `audit.jsonl` holds
    /// the new event.
    #[test]
    fn append_rotates_at_max_bytes() {
        let tmp = tempdir().unwrap();
        let log_path = tmp.path().join(AUDIT_LOG_FILENAME);
        let rotated = tmp.path().join(AUDIT_LOG_ROTATED);

        // Pre-fill the live log to exactly the cap with a sentinel
        // byte that cannot collide with valid JSON output (so a
        // naive append would either fail or produce malformed
        // JSONL — either way, the test catches it).
        let cap: usize = AUDIT_LOG_MAX_BYTES as usize;
        std::fs::write(&log_path, vec![b'x'; cap]).unwrap();

        // Seed the rotated file so we can prove the rotation
        // *replaced* it (rather than appending or skipping).
        std::fs::write(&rotated, "old-rotated-content\n").unwrap();

        let event = AuditEvent {
            ts_unix: 1_700_000_000.0,
            agent_id: AgentId("a-rotation".into()),
            path: PathBuf::from("/x.rs"),
            claim_set: vec![],
            racers: vec![],
            plan_revision: None,
            landed_revision: 1,
        };
        append_edit_event(tmp.path(), &event).unwrap();

        // The rotated sibling now holds the sentinel (cap bytes
        // exactly), and the prior seed is gone.
        let rotated_len = std::fs::metadata(&rotated).unwrap().len() as usize;
        assert_eq!(
            rotated_len, cap,
            "rotated file should hold the full sentinel, got {rotated_len} bytes"
        );

        // The live file is a fresh, JSONL-valid file with one line.
        let current = std::fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = current.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            1,
            "live log should hold exactly the new event, got: {current}"
        );
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["ts_unix"], serde_json::json!(1_700_000_000.0));
        assert_eq!(parsed["agent_id"], serde_json::json!("a-rotation"));
    }

    /// `read_audit_log` with `Some(since_unix)` returns only events
    /// whose `ts_unix >= since_unix`. Two events with timestamps on
    /// either side of the cutoff; the older is filtered, the newer
    /// is returned.
    #[test]
    fn read_returns_events_newer_than_since() {
        let tmp = tempdir().unwrap();
        let old = sample_event(100.0, "old-agent");
        let newer = sample_event(200.0, "newer-agent");
        append_edit_event(tmp.path(), &old).unwrap();
        append_edit_event(tmp.path(), &newer).unwrap();

        let events = read_audit_log(tmp.path(), Some(150.0)).unwrap();
        assert_eq!(events.len(), 1, "expected only the newer event");
        assert!((events[0].ts_unix - 200.0).abs() < 0.001);
        assert_eq!(events[0].agent_id.0, "newer-agent");

        // No cutoff returns both, in append order.
        let all = read_audit_log(tmp.path(), None).unwrap();
        assert_eq!(all.len(), 2);
        assert!((all[0].ts_unix - 100.0).abs() < 0.001);
        assert!((all[1].ts_unix - 200.0).abs() < 0.001);
    }
}
