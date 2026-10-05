//! Sequential (non-loom) checks of the residency rules on the real
//! `SnapshotManager`; the interleavings are `manager_verification.rs` (loom)
//! and `docs/formal/SnapshotInstallHold*.tla`. These pin what a single thread
//! can observe: holds are counted, the cap holds, the LRU *unheld* entry is
//! the one evicted, and a fully held cache answers `busy` instead of growing.
use super::*;
use std::path::PathBuf;
use std::sync::atomic::Ordering as O;

fn fed(dir: &std::path::Path, id: &str, last_used: i64) -> Arc<SnapshotFederation> {
    Arc::new(SnapshotFederation {
        snapshot_id: id.into(),
        backend: Arc::new(PetgraphBackend::ephemeral(dir)),
        holds: Mutex::new(Vec::new()),
        residency: Arc::new(ResidencyTracker::new()),
        contract_index: parking_lot::RwLock::new(None),
        last_used_unix: Mutex::new(last_used),
        held: AtomicUsize::new(0),
        data_dir: PathBuf::from("."),
    })
}

fn manager(dir: &std::path::Path, cap: usize) -> Arc<SnapshotManager> {
    SnapshotManager::with_cap(dir, IndexCache::new(dir), cap)
}

fn ids(m: &SnapshotManager) -> Vec<String> {
    let mut v: Vec<String> = m.resident.lock().keys().cloned().collect();
    v.sort();
    v
}

#[test]
fn hold_if_resident_is_none_when_absent_and_counts_holds_when_present() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path(), 3);
    assert!(m.hold_if_resident("a").is_none());
    let a = fed(dir.path(), "a", 10);
    m.install_resident(a.clone(), 0).unwrap();
    assert_eq!(
        a.held.load(O::SeqCst),
        0,
        "install_resident drops its own hold"
    );
    let (got, g1) = m.hold_if_resident("a").expect("resident");
    assert!(Arc::ptr_eq(&got, &a));
    assert_eq!(a.held.load(O::SeqCst), 1);
    let (_, g2) = m.hold_if_resident("a").unwrap();
    assert_eq!(a.held.load(O::SeqCst), 2, "holds are counted, not a flag");
    drop(g1);
    assert_eq!(
        a.held.load(O::SeqCst),
        1,
        "one release leaves the other hold standing"
    );
    drop(g2);
    assert_eq!(a.held.load(O::SeqCst), 0);
    assert!(m.hold_if_resident("zzz").is_none());
}

#[test]
fn install_below_the_cap_evicts_nothing_and_at_the_cap_evicts_the_lru_unheld() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path(), 2);
    let (old, new) = (fed(dir.path(), "old", 100), fed(dir.path(), "new", 200));
    m.install_resident(old.clone(), 0).unwrap();
    m.install_resident(new.clone(), 0).unwrap();
    assert_eq!(
        ids(&m),
        vec!["new", "old"],
        "below the cap nothing is evicted"
    );
    // Releasing a hold stamps `last_used = now` (one-second resolution), so pin
    // the order explicitly: entries touched within the same second are otherwise
    // indistinguishable and the eviction among them is arbitrary.
    *old.last_used_unix.lock() = 100;
    *new.last_used_unix.lock() = 200;
    let guard = m
        .install_resident_held(fed(dir.path(), "third", 300), 0)
        .unwrap();
    assert_eq!(
        ids(&m),
        vec!["new", "third"],
        "the least recently used entry goes"
    );
    drop(guard);
}

#[test]
fn a_held_entry_is_never_the_one_evicted_even_if_it_is_the_oldest() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path(), 2);
    let (oldest, newer) = (fed(dir.path(), "oldest", 1), fed(dir.path(), "newer", 500));
    m.install_resident(oldest.clone(), 0).unwrap();
    m.install_resident(newer.clone(), 0).unwrap();
    let (_, hold) = m.hold_if_resident("oldest").unwrap();
    *oldest.last_used_unix.lock() = 1;
    *newer.last_used_unix.lock() = 500;
    m.install_resident(fed(dir.path(), "incoming", 900), 0)
        .unwrap();
    assert_eq!(
        ids(&m),
        vec!["incoming", "oldest"],
        "the held oldest survives; the unheld newer one went"
    );
    drop(hold);
}

#[test]
fn when_every_entry_is_held_install_is_busy_and_the_cache_does_not_grow() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path(), 1);
    let held = m
        .install_resident_held(fed(dir.path(), "a", 10), 0)
        .unwrap();
    let t = std::time::Instant::now();
    let busy = m
        .install_resident(fed(dir.path(), "b", 20), 0)
        .expect_err("all slots held");
    assert_eq!(busy.retry_after_ms, 250);
    assert!(
        t.elapsed() < std::time::Duration::from_millis(500),
        "wait_ms = 0 must not block"
    );
    assert_eq!(ids(&m), vec!["a"], "busy leaves the cache as it was");
    drop(held);
    m.install_resident(fed(dir.path(), "b", 20), 0)
        .expect("free once released");
    assert_eq!(ids(&m), vec!["b"]);
}

#[test]
fn a_waiting_install_succeeds_when_a_hold_is_released_within_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path(), 1);
    let held = m
        .install_resident_held(fed(dir.path(), "a", 10), 0)
        .unwrap();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(150));
        drop(held);
    });
    let t = std::time::Instant::now();
    m.install_resident(fed(dir.path(), "b", 20), 3_000)
        .expect("released in time");
    assert!(
        t.elapsed() >= std::time::Duration::from_millis(100),
        "did not actually wait"
    );
    assert!(
        t.elapsed() < std::time::Duration::from_millis(2_500),
        "waited out the whole window"
    );
    releaser.join().unwrap();
    assert_eq!(ids(&m), vec!["b"]);
}

#[test]
fn evict_resident_reports_whether_it_removed_something() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path(), 2);
    m.install_resident(fed(dir.path(), "a", 1), 0).unwrap();
    assert!(m.evict_resident("a"));
    assert!(!m.evict_resident("a"));
    assert!(ids(&m).is_empty());
}
