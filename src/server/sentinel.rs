//! The `O_EXCL` sentinel-file primitive shared by lain's two file locks.
//!
//! Both [`presence_lock`](crate::server::presence_lock) (an advisory
//! *claim* lock, holding agent identity and intent) and
//! [`state_lock`](crate::server::state_lock) (a plain critical section
//! around the presence state file) are built on the same three
//! mechanics: create a sentinel atomically, treat one whose mtime is
//! older than a TTL as abandoned by a dead holder, and remove it to
//! release.
//!
//! Only the *mechanism* lives here. The policies differ and stay with
//! their callers: a claim lock reports who is holding it and expires in
//! seconds, while a critical section retries to a deadline and then
//! proceeds unlocked. Sharing the policy would have forced one of those
//! shapes onto the other.

use std::path::Path;
use std::time::{Duration, SystemTime};

/// Outcome of a single acquisition attempt.
pub enum Acquire {
    /// The sentinel was created; the caller holds it. Carries the open
    /// handle so callers that record holder metadata can write to it
    /// without reopening.
    Acquired(std::fs::File),
    /// A sentinel exists and is within its TTL — someone else holds it.
    Held,
    /// A sentinel exists but is older than the TTL, so its holder is
    /// presumed dead and it may be taken over.
    Stale,
    /// The sentinel could not be created for a reason other than it
    /// already existing (unwritable directory, read-only mount).
    /// Callers decide whether that is fatal; both of lain's locks
    /// degrade rather than fail, because the layer is advisory.
    Unavailable(std::io::Error),
}

/// One atomic attempt to take `sentinel_path`.
///
/// Creates parent directories best-effort first: an unwritable location
/// surfaces as [`Acquire::Unavailable`] from the create itself rather
/// than as a confusing "already held".
pub fn try_acquire(sentinel_path: &Path, ttl: Duration) -> Acquire {
    if let Some(parent) = sentinel_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(sentinel_path)
    {
        Ok(file) => Acquire::Acquired(file),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if is_stale(sentinel_path, SystemTime::now(), ttl) {
                Acquire::Stale
            } else {
                Acquire::Held
            }
        }
        Err(e) => Acquire::Unavailable(e),
    }
}

/// Is the sentinel older than `ttl` as of `now`?
///
/// `now` is a parameter so staleness is testable without backdating a
/// file's mtime, which would need a dependency this crate doesn't carry.
/// A sentinel that cannot be stat'd is *not* stale: absence of evidence
/// must not license taking someone else's lock.
pub fn is_stale(sentinel_path: &Path, now: SystemTime, ttl: Duration) -> bool {
    let Ok(meta) = std::fs::metadata(sentinel_path) else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    now.duration_since(mtime).map(|age| age > ttl).unwrap_or(false)
}

/// Age of the sentinel as of `now`, or `None` if it can't be stat'd.
pub fn age(sentinel_path: &Path, now: SystemTime) -> Option<Duration> {
    let meta = std::fs::metadata(sentinel_path).ok()?;
    let mtime = meta.modified().ok()?;
    now.duration_since(mtime).ok()
}

/// Release by removing the sentinel. Idempotent: a missing file is
/// success, since the goal state is "not held".
pub fn release(sentinel_path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(sentinel_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_caller_acquires_second_sees_held() {
        let tmp = tempfile::tempdir().unwrap();
        let s = tmp.path().join("x.lock");
        assert!(matches!(try_acquire(&s, Duration::from_secs(10)), Acquire::Acquired(_)));
        assert!(matches!(try_acquire(&s, Duration::from_secs(10)), Acquire::Held));
    }

    #[test]
    fn a_sentinel_past_its_ttl_reads_as_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let s = tmp.path().join("x.lock");
        let _ = try_acquire(&s, Duration::from_secs(10));
        let ttl = Duration::from_secs(10);
        assert!(!is_stale(&s, SystemTime::now(), ttl), "a fresh sentinel is a live holder");
        assert!(
            is_stale(&s, SystemTime::now() + ttl + Duration::from_secs(1), ttl),
            "a holder that died must not block its peers forever"
        );
    }

    #[test]
    fn release_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let s = tmp.path().join("x.lock");
        let _ = try_acquire(&s, Duration::from_secs(10));
        release(&s).expect("first release");
        release(&s).expect("releasing an already-released sentinel is success");
        assert!(!s.exists());
    }

    #[test]
    fn a_missing_sentinel_is_not_stale() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_stale(&tmp.path().join("nope.lock"), SystemTime::now(), Duration::from_secs(1)));
    }
}
