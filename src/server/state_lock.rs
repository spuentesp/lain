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
use parking_lot::{FairMutex, FairMutexGuard};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
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
    /// This process's turn, released after the sentinel (fields drop
    /// after `Drop::drop`).
    _turn: Option<FairMutexGuard<'static, ()>>,
}

/// One fair queue per state file for the threads of this process.
///
/// The sentinel is polled, so it is not fair: a thread that just
/// released it takes it again before sleeping waiters wake. With eight
/// agents in one server, a waiter could lose every race for the whole
/// deadline and get `CoordinationError::Unavailable` — seen on macOS CI
/// (`concurrent_agents_contention_benchmark`). Queueing this process's
/// threads first leaves only one of them polling the sentinel, which then
/// only arbitrates between processes. The mutexes are leaked: one per
/// distinct state path for the life of the process.
fn turn_for(path: &Path) -> &'static FairMutex<()> {
    static TURNS: OnceLock<Mutex<HashMap<PathBuf, &'static FairMutex<()>>>> = OnceLock::new();
    let mut turns = TURNS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    turns
        .entry(path.to_path_buf())
        .or_insert_with(|| Box::leak(Box::new(FairMutex::new(()))))
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

/// Acquire the lock for `state_path` with the loaded timing tunables.
/// Always returns a `StateLock` — on timeout it returns one with
/// `held == false` so the caller proceeds unlocked rather than failing.
///
/// Callers that need to fail closed when coordination is impossible
/// should check `StateLock::is_held()` and surface an error to the
/// caller; this entry point keeps the historical "advisory lock
/// that degrades gracefully" semantics for back-compat. New code
/// should prefer [`acquire_with`], which is the same primitive
/// driven by caller-supplied timeouts.
pub fn acquire(state_path: &Path) -> StateLock {
    let cfg = crate::server::tuning::PresenceConfig::default();
    acquire_with(
        state_path,
        cfg.state_lock_acquire_timeout_ms,
        cfg.state_lock_retry_interval_ms,
        cfg.state_lock_stale_after_secs,
    )
}

/// Acquire the lock for `state_path` using the supplied timeouts. Use
/// this when the caller has loaded `tuning.toml` and wants the lock
/// to honour operator overrides — `acquire` always uses
/// `PresenceConfig::default()`, which silently ignores the tuning
/// file. The same advisory semantics apply: on timeout the returned
/// `StateLock` has `held == false` so the caller can branch on
/// `is_held()` and decide whether to fail closed or proceed.
pub fn acquire_with(
    state_path: &Path,
    acquire_timeout_ms: u64,
    retry_interval_ms: u64,
    stale_after_secs: u64,
) -> StateLock {
    let acquire_timeout = Duration::from_millis(acquire_timeout_ms);
    let retry_interval = Duration::from_millis(retry_interval_ms);
    let stale_after = Duration::from_secs(stale_after_secs);
    let path = lock_path_for(state_path);
    let deadline = SystemTime::now() + acquire_timeout;
    let Some(turn) = turn_for(&path).try_lock_for(acquire_timeout) else {
        return StateLock {
            path,
            held: false,
            _turn: None,
        };
    };
    loop {
        match sentinel::try_acquire(&path, stale_after) {
            Acquire::Acquired(mut f) => {
                // Record the owner so a human debugging a stuck lock can
                // see which process to look at.
                let _ = writeln!(f, "{}", std::process::id());
                return StateLock {
                    path,
                    held: true,
                    _turn: Some(turn),
                };
            }
            Acquire::Stale => {
                // The holder died. Remove and retry; if two peers race
                // here, one wins the next create.
                let _ = sentinel::release(&path);
            }
            Acquire::Held => {
                if SystemTime::now() >= deadline {
                    return StateLock {
                        path,
                        held: false,
                        _turn: None,
                    };
                }
                std::thread::sleep(retry_interval);
            }
            // Unwritable state dir, permissions, read-only mount:
            // proceed unlocked rather than breaking presence.
            Acquire::Unavailable(_) => {
                return StateLock {
                    path,
                    held: false,
                    _turn: None,
                }
            }
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
            assert!(
                lock_path_for(&state).exists(),
                "sentinel must exist while held"
            );
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
        assert!(
            lock_path_for(&state).exists(),
            "non-holder must not release"
        );
    }
}
