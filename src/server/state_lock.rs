//! Cross-process critical section around the presence state file.
//!
//! Presence lived only in each process's memory. The MCP stdio
//! transport spawns one server *per client*, so two Claude Code windows
//! on the same repo ran two servers with two registries and zero shared
//! knowledge — while every tool kept answering successfully. Registering
//! on one and listing on the other returned an empty list, and the
//! documented wiring (`--transport stdio`) is exactly the topology where
//! that happens.
//!
//! On a single machine the state file is already the shared medium:
//! `save_pair` / `load_pair` round-trip the whole registry through
//! `<state_dir>/<workspace>.json`. What was missing is (a) re-reading it
//! before acting, so a process sees its peers, and (b) a lock, so a
//! read-modify-write cycle doesn't clobber a peer's concurrent write.
//!
//! The lock is an `O_EXCL` sentinel next to the state file — no new
//! dependency, and the same primitive `presence_lock` already uses for
//! the zero-daemon fallback.
//!
//! **Advisory, never blocking.** If the lock can't be taken within
//! [`ACQUIRE_TIMEOUT`], the caller proceeds without it. A presence
//! registry that occasionally loses a concurrent write is a nuisance; a
//! presence registry that can wedge an agent's session is a much worse
//! failure, and the whole subsystem is advisory to begin with.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long to keep retrying before giving up and proceeding unlocked.
const ACQUIRE_TIMEOUT: Duration = Duration::from_millis(2000);
/// Gap between attempts.
const RETRY_INTERVAL: Duration = Duration::from_millis(20);
/// A sentinel older than this is assumed to belong to a process that
/// died before releasing, and is taken over.
const STALE_AFTER: Duration = Duration::from_secs(10);

/// Sentinel path for a given state file.
pub fn lock_path_for(state_path: &Path) -> PathBuf {
    let mut name = state_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "state".to_string());
    name.push_str(".lock");
    state_path.with_file_name(name)
}

/// Held lock. Releasing on drop matters more than usual here: an early
/// return that leaked the sentinel would stall every peer for
/// `STALE_AFTER` before they took it over.
pub struct StateLock {
    path: PathBuf,
    /// `false` when acquisition timed out and the caller proceeded
    /// anyway — dropping must not remove someone else's sentinel.
    held: bool,
}

impl StateLock {
    /// True when the lock was actually acquired. Callers don't need to
    /// branch on it; it exists for tests and diagnostics.
    pub fn is_held(&self) -> bool {
        self.held
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        if self.held {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Acquire the lock for `state_path`, retrying until [`ACQUIRE_TIMEOUT`].
///
/// Always returns a `StateLock` — on timeout it returns one with
/// `held == false` so the caller proceeds unlocked rather than failing.
pub fn acquire(state_path: &Path) -> StateLock {
    let path = lock_path_for(state_path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let deadline = SystemTime::now() + ACQUIRE_TIMEOUT;
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                // Record the owner so a human debugging a stuck lock can
                // see which process to look at.
                let _ = writeln!(f, "{}", std::process::id());
                return StateLock { path, held: true };
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if is_stale_at(&path, SystemTime::now()) {
                    // The holder died. Remove and retry; if two peers
                    // race here, one wins the `create_new` below.
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                if SystemTime::now() >= deadline {
                    return StateLock { path, held: false };
                }
                std::thread::sleep(RETRY_INTERVAL);
            }
            Err(_) => {
                // Unwritable state dir, permissions, read-only mount:
                // proceed unlocked rather than breaking presence.
                return StateLock { path, held: false };
            }
        }
    }
}

/// `now` is injected so staleness is testable without backdating a
/// file's mtime, which would need a dependency this crate doesn't carry.
fn is_stale_at(path: &Path, now: SystemTime) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    now.duration_since(mtime)
        .map(|age| age > STALE_AFTER)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_path_sits_next_to_the_state_file() {
        let p = lock_path_for(Path::new("/state/lain/repos-ab12.json"));
        assert_eq!(p, PathBuf::from("/state/lain/repos-ab12.json.lock"));
    }

    #[test]
    fn acquire_and_release_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("s.json");
        {
            let lock = acquire(&state);
            assert!(lock.is_held());
            assert!(lock_path_for(&state).exists(), "sentinel must exist while held");
        }
        assert!(
            !lock_path_for(&state).exists(),
            "sentinel must be removed on drop, or peers stall until it goes stale"
        );
    }

    #[test]
    fn second_acquire_times_out_and_proceeds_unlocked() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("s.json");
        let _held = acquire(&state);
        // The point of the design: a contended lock degrades to
        // "proceed without it", never to a hang or an error.
        let second = acquire(&state);
        assert!(!second.is_held());
        // Dropping the non-holder must not delete the real holder's
        // sentinel.
        drop(second);
        assert!(lock_path_for(&state).exists(), "non-holder must not release");
    }

    #[test]
    fn a_sentinel_older_than_the_window_is_stale() {
        // A process that died holding the lock must not block its peers
        // forever. Rather than backdating the file (which would need a
        // dependency), look at it from a point far enough in the future.
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("s.json");
        let sentinel = lock_path_for(&state);
        std::fs::write(&sentinel, "99999").unwrap();

        let now = SystemTime::now();
        assert!(
            !is_stale_at(&sentinel, now),
            "a freshly written sentinel is a live holder"
        );
        assert!(
            is_stale_at(&sentinel, now + STALE_AFTER + Duration::from_secs(5)),
            "a sentinel past the window must be taken over"
        );
    }

    #[test]
    fn a_missing_sentinel_is_not_stale() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_stale_at(&tmp.path().join("nope.lock"), SystemTime::now()));
    }
}
