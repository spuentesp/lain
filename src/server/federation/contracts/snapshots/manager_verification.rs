//! loom model of the residency protocol on the REAL `SnapshotManager`
//! (`resident` mutex and `held` counter come from `crate::sync`, which is
//! loom's under `--cfg lain_loom`). TLA+ counterpart:
//! `docs/formal/SnapshotInstallHold.tla`, `SnapshotResidency_VariantBC.tla`.
//! Run: `RUSTFLAGS="--cfg lain_loom" cargo test --lib loom_ --release`.
#![cfg(lain_loom)]
use super::*;
use loom::thread;
use std::path::PathBuf;

fn fed(dir: &std::path::Path, id: &str) -> Arc<SnapshotFederation> {
    Arc::new(SnapshotFederation {
        snapshot_id: id.into(),
        backend: Arc::new(PetgraphBackend::ephemeral(dir)),
        holds: Mutex::new(Vec::new()),
        residency: Arc::new(ResidencyTracker::new()),
        contract_index: parking_lot::RwLock::new(None),
        last_used_unix: Mutex::new(now_unix()),
        held: AtomicUsize::new(0),
        data_dir: PathBuf::from("."),
    })
}

fn manager(dir: &std::path::Path, cap: usize) -> Arc<SnapshotManager> {
    SnapshotManager::with_cap(dir, IndexCache::new(dir), cap)
}

/// A held snapshot is never evicted, and the cap is never exceeded, however
/// an installing builder races a competing install at capacity.
#[test]
fn loom_held_snapshot_is_never_evicted_and_cap_holds() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    loom::model(move || {
        let mgr = manager(&root, 1);
        let a = {
            let (mgr, d) = (mgr.clone(), root.clone());
            thread::spawn(move || {
                // Busy is legitimate when the other install momentarily holds
                // the only slot (wait_ms = 0); only a *successful* install
                // carries the guarantee.
                if let Ok(guard) = mgr.install_resident_held(fed(&d, "a"), 0) {
                    assert!(
                        mgr.resident.lock().contains_key("a"),
                        "held snapshot was evicted (NoEvictionOfHeld)"
                    );
                    drop(guard);
                }
            })
        };
        let b = {
            let (mgr, d) = (mgr.clone(), root.clone());
            thread::spawn(move || {
                let _ = mgr.install_resident(fed(&d, "b"), 0); // Ok, or Busy while "a" is held
            })
        };
        a.join().unwrap();
        b.join().unwrap();
        assert!(mgr.resident.lock().len() <= 1, "CapBound");
    });
}

/// Negative control: the pre-fix order (insert, drop the lock, hold later)
/// must be caught by the same model.
#[test]
#[should_panic(expected = "NoEvictionOfHeld")]
fn loom_insert_then_late_hold_is_caught() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    loom::model(move || {
        let mgr = manager(&root, 1);
        let a = {
            let (mgr, d) = (mgr.clone(), root.clone());
            thread::spawn(move || {
                let f = fed(&d, "a");
                mgr.install_resident(Arc::clone(&f), 0).expect("room");
                let guard = HoldGuard::new(f, Arc::clone(&mgr.residency_notify)); // late hold
                assert!(
                    mgr.resident.lock().contains_key("a"),
                    "NoEvictionOfHeld: held snapshot not resident"
                );
                drop(guard);
            })
        };
        let b = {
            let (mgr, d) = (mgr.clone(), root.clone());
            thread::spawn(move || {
                let _ = mgr.install_resident(fed(&d, "b"), 0);
            })
        };
        a.join().unwrap();
        b.join().unwrap();
    });
}
