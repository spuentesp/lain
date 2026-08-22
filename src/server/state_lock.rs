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
//! The lock is an `O_EXCL` sentinel next to the state file, built on
//! [`crate::server::sentinel`] — the same primitive `presence_lock`
//! uses for the zero-daemon fallback. Only the policy differs: this is
//! a critical section that retries to a deadline and then proceeds
//! unlocked, where a claim lock reports its holder and expires in
//! seconds.
//!
//! **Advisory, never blocking.** If the lock can't be taken within
//! [`ACQUIRE_TIMEOUT`], the caller proceeds without it. A presence
//! registry that occasionally loses a concurrent write is a nuisance; a
//! presence registry that can wedge an agent's session is a much worse
//! failure, and the whole subsystem is advisory to begin with.

use crate::server::sentinel::{self, Acquire};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long to keep retrying before giving up and proceeding unlocked.
/// Gap between attempts.
/// A sentinel older than this is assumed to belong to a process that
/// died before releasing, and is taken over.

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
            let _ = sentinel::release(&self.path);
        }
    }
}

/// Acquire the lock for `state_path`, retrying until [`ACQUIRE_TIMEOUT`].
///
/// Always returns a `StateLock` — on timeout it returns one with
/// `held == false` so the caller proceeds unlocked rather than failing.
pub fn acquire(state_path: &Path) -> StateLock {
    // Read from `PresenceConfig` rather than local constants, so every
    // presence-related timing is declared in one place with the rest of
    // lain's tunables.
    let cfg = crate::server::tuning::PresenceConfig::default();
    let acquire_timeout = Duration::from_millis(cfg.state_lock_acquire_timeout_ms);
    let retry_interval = Duration::from_millis(cfg.state_lock_retry_interval_ms);
    let stale_after = Duration::from_secs(cfg.state_lock_stale_after_secs);
    let path = lock_path_for(state_path);
    let deadline = SystemTime::now() + acquire_timeout;
    loop {
        match sentinel::try_acquire(&path, stale_after) {
            Acquire::Acquired(mut f) => {
                // Record the owner so a human debugging a stuck lock can
                // see which process to look at.
                let _ = writeln!(f, "{}", std::process::id());
                return StateLock { path, held: true };
            }
            Acquire::Stale => {
                // The holder died. Remove and retry; if two peers race
                // here, one wins the next create.
                let _ = sentinel::release(&path);
            }
            Acquire::Held => {
                if SystemTime::now() >= deadline {
                    return StateLock { path, held: false };
                }
                std::thread::sleep(retry_interval);
            }
            // Unwritable state dir, permissions, read-only mount:
            // proceed unlocked rather than breaking presence.
            Acquire::Unavailable(_) => return StateLock { path, held: false },
        }
    }
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

}
