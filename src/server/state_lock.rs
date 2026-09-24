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
//! The lock is an OS advisory lock (`flock` / `LockFileEx`) on a file next
//! to the state file, retried to a deadline. The kernel releases it when
//! the holder exits or dies, so there is no staleness to judge. (It was an
//! `O_EXCL` sentinel with a stale-takeover, which could let two peers in
//! at once; see [`StateLock`].)
//!
//! **Never blocking.** If the lock can't be taken before the deadline,
//! `acquire` returns an unheld lock; `with_shared_presence` then reports
//! `CoordinationError::Unavailable` so the agent retries, rather than
//! wedging its session.

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

/// Held lock: an OS advisory lock (`flock` / `LockFileEx`) on
/// `<state>.lock`, released when the handle closes — on drop, or by the
/// kernel if the process dies.
///
/// This was an `O_EXCL` sentinel file taken over after `stale_after`
/// seconds. The takeover was check-then-act: two peers could both judge a
/// dead holder's file stale, one delete-and-recreate it, the other then
/// delete the *new* holder's file, and both proceed — two agents granted
/// the same exclusive claim (2 in ~1000 trials with a planted stale
/// lock). A kernel lock needs no staleness guess and no takeover.
pub struct StateLock {
    #[allow(dead_code)]
    path: PathBuf,
    /// `false` when acquisition timed out and the caller proceeded anyway.
    held: bool,
    /// The locked handle; closing it releases the lock.
    _file: Option<std::fs::File>,
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
    _stale_after_secs: u64,
) -> StateLock {
    let acquire_timeout = Duration::from_millis(acquire_timeout_ms);
    let retry_interval = Duration::from_millis(retry_interval_ms);
    let path = lock_path_for(state_path);
    let deadline = SystemTime::now() + acquire_timeout;
    let unlocked = |path: PathBuf| StateLock {
        path,
        held: false,
        _file: None,
        _turn: None,
    };
    let Some(turn) = turn_for(&path).try_lock_for(acquire_timeout) else {
        return unlocked(path);
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Never created exclusively and never deleted: the file only carries
    // the lock, so a leftover one from a crash blocks nobody.
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
    {
        Ok(f) => f,
        // Unwritable state dir, read-only mount: proceed unlocked rather
        // than breaking presence.
        Err(_) => return unlocked(path),
    };
    loop {
        match file.try_lock() {
            Ok(()) => {
                // Record the owner so a human debugging a stuck lock can see
                // which process to look at.
                let _ = file.set_len(0);
                let _ = writeln!(&file, "{}", std::process::id());
                return StateLock {
                    path,
                    held: true,
                    _file: Some(file),
                    _turn: Some(turn),
                };
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                if SystemTime::now() >= deadline {
                    return unlocked(path);
                }
                std::thread::sleep(retry_interval);
            }
            Err(std::fs::TryLockError::Error(_)) => return unlocked(path),
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
            // Another handle on the file cannot take the lock meanwhile.
            let other = std::fs::OpenOptions::new()
                .write(true)
                .open(lock_path_for(&state))
                .unwrap();
            assert!(matches!(
                other.try_lock(),
                Err(std::fs::TryLockError::WouldBlock)
            ));
        }
        let again = acquire(&state);
        assert!(again.is_held(), "released on drop");
    }

    /// A lock file left by a process that died is no obstacle: the lock
    /// went with the process.
    #[test]
    fn a_leftover_lock_file_does_not_block() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("s.json");
        std::fs::write(lock_path_for(&state), "12345\n").unwrap();
        assert!(acquire(&state).is_held());
    }

    #[test]
    fn second_acquire_times_out_and_proceeds_unlocked() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("s.json");
        let held = acquire(&state);
        // The point of the design: a contended lock degrades to
        // "proceed without it", never to a hang or an error.
        let second = acquire(&state);
        assert!(!second.is_held());
        // Dropping the non-holder leaves the holder's lock in place.
        drop(second);
        let other = std::fs::OpenOptions::new()
            .write(true)
            .open(lock_path_for(&state))
            .unwrap();
        assert!(matches!(
            other.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(held);
    }
}
