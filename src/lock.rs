//! Workspace lock around `<workspace>/.lain/server.lock`.
//!
//! Owner processes hold an exclusive `flock` for their lifetime. Sidecar
//! processes briefly take a shared `flock` to verify the owner is alive,
//! then drop the lock. The on-disk file still carries the owner's
//! `pid:port` for debugging.

use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use crate::error::LainError;

#[derive(Debug)]
pub struct WorkspaceLock {
    path: PathBuf,
}

impl WorkspaceLock {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(unix)]
    pub fn acquire_exclusive(&self) -> Result<ExclusiveGuard, LainError> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.path)
            .map_err(LainError::from)?;
        // Non-blocking: second call must fail rather than hang the caller.
        f.try_lock_exclusive()
            .map_err(|e| LainError::Other(format!("workspace lock held: {e}")))?;
        Ok(ExclusiveGuard(f))
    }

    #[cfg(not(unix))]
    pub fn acquire_exclusive(&self) -> Result<ExclusiveGuard, LainError> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.path)
            .map_err(LainError::from)?;
        Ok(ExclusiveGuard(f))
    }

    #[cfg(unix)]
    pub fn acquire_shared<'a>(&'a self) -> Result<SharedGuard<'a>, LainError> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.path)
            .map_err(LainError::from)?;
        // Non-blocking: an exclusive holder must make the shared acquire fail
        // so the sidecar can see the owner is unreachable.
        f.try_lock_shared()
            .map_err(|e| LainError::Other(format!("workspace lock contended: {e}")))?;
        Ok(SharedGuard(&self.path, f))
    }

    #[cfg(not(unix))]
    pub fn acquire_shared<'a>(&'a self) -> Result<SharedGuard<'a>, LainError> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.path)
            .map_err(LainError::from)?;
        Ok(SharedGuard(&self.path, f))
    }

    pub fn owner_pid(&self) -> Option<u32> {
        self.read_owner_pid()
    }

    pub fn read_owner_pid(&self) -> Option<u32> {
        let s = std::fs::read_to_string(&self.path).ok()?;
        s.split(':').next()?.trim().parse().ok()
    }

    pub fn write_owner_pid(&self, pid: u32, port: u16) -> Result<(), LainError> {
        std::fs::write(&self.path, format!("{pid}:{port}\n")).map_err(LainError::from)
    }
}

pub struct ExclusiveGuard(File);

impl Drop for ExclusiveGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = FileExt::unlock(&self.0);
        }
        // On non-Unix, the guard is a no-op; the file is closed when dropped.
    }
}

#[allow(dead_code)]
pub struct SharedGuard<'a>(&'a Path, File);

impl Drop for SharedGuard<'_> {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = FileExt::unlock(&self.1);
        }
        // On non-Unix, the guard is a no-op; the file is closed when dropped.
    }
}

#[cfg(test)]
mod tests {
    use super::WorkspaceLock;
    use std::fs;

    fn temp_path(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(name);
        (dir, path)
    }

    #[test]
    fn exclusive_lock_blocks_second_attempt() {
        let (_dir, path) = temp_path("lk");
        fs::write(&path, b"").unwrap();
        let lock = WorkspaceLock::new(path.clone());
        let _g1 = lock.acquire_exclusive().expect("first");
        let r = lock.acquire_exclusive();
        assert!(r.is_err(), "second exclusive must fail");
    }

    #[test]
    fn shared_locks_coexist() {
        let (_dir, path) = temp_path("lk2");
        fs::write(&path, b"").unwrap();
        let lock = WorkspaceLock::new(path.clone());
        let g1 = lock.acquire_shared().expect("first");
        let _g2 = lock.acquire_shared().expect("second");
        drop(g1);
    }

    #[test]
    fn shared_blocks_exclusive_and_vice_versa() {
        let (_dir, path) = temp_path("lk3");
        fs::write(&path, b"").unwrap();
        let lock = WorkspaceLock::new(path.clone());
        let _g = lock.acquire_shared().expect("shared");
        assert!(lock.acquire_exclusive().is_err());
    }

    #[test]
    fn text_file_round_trip() {
        let (_dir, path) = temp_path("lk4");
        let lock = WorkspaceLock::new(path.clone());
        lock.write_owner_pid(1234, 9999).unwrap();
        let got = lock.read_owner_pid();
        assert_eq!(got, Some(1234));
    }

    /// Coexistence contract from Task 3's review fix: a second owner must
    /// be rejected with a clear `workspace lock held` error while a sidecar
    /// (which only reads the file) is admitted and can observe the owner's
    /// pid.
    ///
    /// flock semantics: while an exclusive holder exists, even a *shared*
    /// acquire will fail with `LOCK_NB`/WouldBlock. The sidecar interprets
    /// that contention as the signal that an owner is alive, then drops
    /// the failed attempt and reads the pid from the file.
    #[test]
    fn second_owner_rejected_sidecar_admitted() {
        let (_dir, path) = temp_path("lk5");
        let lock = WorkspaceLock::new(path.clone());

        // First owner takes the exclusive flock and writes its pid.
        let _owner = lock.acquire_exclusive().expect("first owner");
        lock.write_owner_pid(4242, 9999).expect("write pid");

        // Second owner must fail fast with a clear message.
        let second = lock.acquire_exclusive();
        assert!(second.is_err(), "second owner must fail");
        let msg = format!("{}", second.err().unwrap());
        assert!(
            msg.contains("workspace lock held"),
            "second owner error should mention the held lock, got: {msg}"
        );

        // Sidecar path: its shared acquire is blocked by the exclusive
        // holder — that's the "owner is alive" signal. The sidecar then
        // reads the pid from the on-disk file directly.
        let shared_attempt = lock.acquire_shared();
        assert!(
            shared_attempt.is_err(),
            "sidecar's shared acquire must fail while an exclusive holder is alive"
        );
        let shared_msg = format!("{}", shared_attempt.err().unwrap());
        assert!(
            shared_msg.contains("workspace lock contended"),
            "shared attempt should report contention, got: {shared_msg}"
        );

        // Even though the flock attempt failed, the sidecar is admitted
        // because the pid file is readable and parses to a real number.
        assert_eq!(lock.read_owner_pid(), Some(4242));

        // And once the owner drops its exclusive lock, a sidecar can take
        // a real shared flock (verifies the no-owner case).
        drop(_owner);
        let _shared = lock.acquire_shared().expect("shared after owner gone");
    }
}
