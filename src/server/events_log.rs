//! Durable SSE event log (P1 #2).
//!
//! Captures every `PresenceEvent` broadcast on the SSE channel to a
//! per-server JSONL file (`<state_dir>/events.jsonl`), assigns each a
//! monotonic `event_id: u64`, and supports replay-after-id queries.
//!
//! This is the durable companion to the volatile `tokio::sync::broadcast`
//! channel: when a subscriber reconnects with `Last-Event-ID: N` (the
//! SSE standard for resumption), the server replays every event with
//! `event_id > N` from the file before yielding from the live bus.
//!
//! Scope: a single rotation cap (50 MB) for parity with `audit.jsonl`;
//! `events.jsonl.1` is the rotated file. We don't currently use a cap
//! that's smaller than 50 MB because the LLM's value is "did anything
//! happen during my absence" — a few stale events from before the
//! rotation is acceptable.
//!
//! File format: one JSON object per line, `event_id: u64` and the
//! `PresenceEvent` value as a tagged enum (`{"event_id": N, "event": {...}}`).
//! Malformed lines are skipped silently (same policy as the audit log
//! reader) so a single bad write doesn't poison the entire replay.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use parking_lot::Mutex;

use crate::server::presence::PresenceEvent;

pub const EVENTS_LOG_FILENAME: &str = "events.jsonl";
pub const EVENTS_LOG_ROTATED: &str = "events.jsonl.1";
pub const EVENTS_LOG_MAX_BYTES: u64 = 50 * 1024 * 1024;

/// Append-only event log with replay-after-id queries. Cheap to clone
/// (`Arc<Mutex<...>>` underneath) so the LainServer can hand a handle
/// to the SSE handler and the broadcast bus.
#[derive(Debug)]
pub struct EventsLog {
    state_dir: PathBuf,
    file: Mutex<File>,
    next_id: Mutex<u64>,
}

impl EventsLog {
    /// Open the log in `state_dir` and load the existing `next_id`
    /// by scanning the file for the highest `event_id` already written.
    /// If the file doesn't exist, `next_id` starts at 1 (matching the
    /// SSE `Last-Event-ID` convention where the first id is `1`).
    pub fn open(state_dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(state_dir)?;
        let path = state_dir.join(EVENTS_LOG_FILENAME);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let next_id = read_max_event_id(&path).unwrap_or(0) + 1;
        Ok(EventsLog {
            state_dir: state_dir.to_path_buf(),
            file: Mutex::new(file),
            next_id: Mutex::new(next_id),
        })
    }

    /// Append a `PresenceEvent` to the log. Returns the assigned
    /// `event_id`. Performs the 50 MB rotation if the file is at
    /// capacity. Best-effort: a write failure is logged and the
    /// counter is still incremented (the in-memory state is the
    /// authoritative id for the live bus; the file is just a
    /// durability layer).
    pub fn append(&self, event: &PresenceEvent) -> u64 {
        let id = {
            let mut counter = self.next_id.lock();
            let id = *counter;
            *counter += 1;
            id
        };
        let payload = match serde_json::to_string(event) {
            Ok(s) => s,
            Err(_) => return id,  // skip persistence on serialize failure
        };
        let line = format!("{}\t{}\n", id, payload);
        let mut guard = self.file.lock();
        // Rotation: if the file is at-or-above the cap, rename to .1
        // (overwriting any prior rotated file) and reopen.
        let needs_rotate = guard
            .metadata()
            .map(|m| m.len() >= EVENTS_LOG_MAX_BYTES)
            .unwrap_or(false);
        if needs_rotate {
            let rotated = self.state_dir.join(EVENTS_LOG_ROTATED);
            let _ = std::fs::remove_file(&rotated);
            let _ = std::fs::rename(self.state_dir.join(EVENTS_LOG_FILENAME), &rotated);
            if let Ok(new_file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.state_dir.join(EVENTS_LOG_FILENAME))
            {
                *guard = new_file;
            }
        }
        if let Err(e) = guard.write_all(line.as_bytes()) {
            tracing::warn!("EventsLog append failed: {e}");
        }
        id
    }

    /// Iterate over events with `event_id > last_id`, in id order.
    /// Reads from `events.jsonl` and `events.jsonl.1` (rotation order).
    /// Skips malformed lines.
    pub fn replay_after(&self, last_id: u64) -> impl Iterator<Item = (u64, PresenceEvent)> {
        let mut out: Vec<(u64, PresenceEvent)> = Vec::new();
        for name in [EVENTS_LOG_FILENAME, EVENTS_LOG_ROTATED] {
            let path = self.state_dir.join(name);
            if !path.exists() {
                continue;
            }
            let Ok(file) = File::open(&path) else { continue };
            // Collect all (id, payload) tuples up front so the borrow on
            // the file (and the lines iterator) doesn't extend across
            // the in-loop deserialize_json calls.
            let entries: Vec<(u64, String)> = BufReader::new(file)
                .lines()
                .map_while(Result::ok)
                .filter_map(|line| {
                    let (id_str, rest) = line.split_once('\t')?;
                    let id = id_str.parse::<u64>().ok()?;
                    if id <= last_id { return None; }
                    Some((id, rest.to_string()))
                })
                .collect();
            // `PresenceEvent` is fully owned (PathBuf/String fields), so the
            // deserialized value doesn't borrow from `payload` and can be
            // moved into `out` directly.
            for (id, payload) in entries {
                if let Ok(ev) = serde_json::from_str::<PresenceEvent>(&payload) {
                    out.push((id, ev));
                }
            }
        }
        out.sort_by_key(|(id, _)| *id);
        out.into_iter()
    }
}

/// Scan the events files and return the highest `event_id` found
/// across both. Returns `None` if the file doesn't exist or is
/// unreadable.
fn read_max_event_id(path: &Path) -> Option<u64> {
    let file = File::open(path).ok()?;
    let mut max: u64 = 0;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Some((id, _)) = line.split_once('\t') else { continue };
        if let Ok(n) = id.parse::<u64>() {
            if n > max { max = n; }
        }
    }
    Some(max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::presence::{AgentId, AgentKind, AgentMode};
    use std::path::PathBuf;

    fn tmp() -> tempfile::TempDir { tempfile::tempdir().unwrap() }

    #[test]
    fn first_event_gets_id_1() {
        let dir = tmp();
        let log = EventsLog::open(dir.path()).unwrap();
        let sess = crate::server::presence::AgentSession {
            id: AgentId("a".into()),
            name: "a".into(),
            kind: AgentKind::ClaudeCode,
            mode: AgentMode::Interactive,
            pid: None,
            parent_session_id: None,
            session_token: "t".into(),
            started_at: std::time::SystemTime::now(),
            last_heartbeat: std::time::SystemTime::now(),
        };
        let id = log.append(&PresenceEvent::AgentJoined(sess));
        assert_eq!(id, 1, "first event in a fresh log is id=1 (SSE convention)");
    }

    #[test]
    fn replay_after_last_id() {
        let dir = tmp();
        let log = EventsLog::open(dir.path()).unwrap();
        let sess = crate::server::presence::AgentSession {
            id: AgentId("a".into()),
            name: "a".into(),
            kind: AgentKind::ClaudeCode,
            mode: AgentMode::Interactive,
            pid: None,
            parent_session_id: None,
            session_token: "t".into(),
            started_at: std::time::SystemTime::now(),
            last_heartbeat: std::time::SystemTime::now(),
        };
        let id1 = log.append(&PresenceEvent::AgentJoined(sess.clone()));
        let id2 = log.append(&PresenceEvent::AgentLeft(AgentId("a".into())));
        let id3 = log.append(&PresenceEvent::AgentJoined(sess));
        let replayed: Vec<u64> = log.replay_after(1).map(|(id, _)| id).collect();
        assert_eq!(replayed, vec![id2, id3], "replay_after(1) returns events with id > 1, in order");
    }

    #[test]
    fn next_id_resumes_after_restart() {
        let dir = tmp();
        let id1;
        {
            let log = EventsLog::open(&dir.path().join("a")).unwrap();
            let sess = crate::server::presence::AgentSession {
                id: AgentId("a".into()),
                name: "a".into(),
                kind: AgentKind::ClaudeCode,
                mode: AgentMode::Interactive,
                pid: None,
                parent_session_id: None,
                session_token: "t".into(),
                started_at: std::time::SystemTime::now(),
                last_heartbeat: std::time::SystemTime::now(),
            };
            id1 = log.append(&PresenceEvent::AgentJoined(sess));
            assert_eq!(id1, 1);
        }
        // Simulate restart: re-open the log in the same dir.
        let log2 = EventsLog::open(&dir.path().join("a")).unwrap();
        let sess = crate::server::presence::AgentSession {
            id: AgentId("b".into()),
            name: "b".into(),
            kind: AgentKind::ClaudeCode,
            mode: AgentMode::Interactive,
            pid: None,
            parent_session_id: None,
            session_token: "t".into(),
            started_at: std::time::SystemTime::now(),
            last_heartbeat: std::time::SystemTime::now(),
        };
        let id2 = log2.append(&PresenceEvent::AgentJoined(sess));
        assert_eq!(id2, 2, "after restart, next_id resumes from the max in the file + 1");
        let _ = PathBuf::new();  // silence unused-import warning if any
    }
}
