//! Crash-consistency tests for the graph WAL, through the real
//! `GraphDatabase`. Each test states a property the WAL must keep:
//!
//! * replay is finite and applies each logged op exactly once;
//! * a torn tail does not poison frames appended after recovery;
//! * WAL order equals in-memory mutation order under concurrent writers;
//! * a checkpoint never loses an op that was acknowledged while it ran;
//! * the WAL does not make bulk inserts pathologically slow.
use super::*;
use crate::graph::GraphDatabase;
use crate::schema::{GraphNode, NodeType};
use std::sync::{mpsc, Arc};
use std::time::Duration;

fn node(name: &str) -> GraphNode {
    GraphNode::new(
        NodeType::Function,
        name.to_string(),
        format!("/src/{name}.rs"),
    )
}

fn frames(path: &Path) -> usize {
    replay(&wal_path_for(path), |_| Ok(())).unwrap()
}

/// Run `f` with a watchdog: an unbounded loop must fail the test, not hang it.
fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(secs))
        .unwrap_or_else(|_| panic!("did not finish within {secs}s (unbounded loop?)"))
}

/// Reloading replays the post-snapshot ops once, then stops, and does not
/// re-log them (replay goes through the mutators, which append to the WAL).
#[test]
fn reload_replays_each_op_exactly_once_and_does_not_relog() {
    let dir = tempfile::tempdir().unwrap();
    let snap = dir.path().join("graph.bin");
    let db = GraphDatabase::new(&snap).unwrap();
    db.upsert_node(node("pre")).unwrap();
    db.save_to_disk_sync().unwrap();
    db.upsert_node(node("post1")).unwrap();
    db.upsert_node(node("post2")).unwrap();
    assert_eq!(frames(&snap), 2);
    drop(db);

    let p = snap.clone();
    let db2 = within(15, move || GraphDatabase::new(&p).unwrap());
    for name in ["pre", "post1", "post2"] {
        let id = node(name).id;
        assert!(
            db2.get_node(&id).unwrap().is_some(),
            "{name} lost on reload"
        );
    }
    // Replay did not re-log, and recovery folded the ops into a fresh
    // snapshot, so the log does not grow across restarts.
    assert_eq!(frames(&snap), 0, "recovery must compact, not re-log");
    assert!(!wal_prev_exists(&snap), "retired log must be discarded");
    // ...and the compacted snapshot alone now carries everything.
    drop(db2);
    let db3 = GraphDatabase::new(&snap).unwrap();
    for name in ["pre", "post1", "post2"] {
        assert!(db3.get_node(&node(name).id).unwrap().is_some());
    }
}

fn wal_prev_exists(snap: &Path) -> bool {
    prev_path_for(&wal_path_for(snap)).exists()
}

/// An edge whose endpoints are missing is dropped by `insert_edges_batch`;
/// it must not be logged, and a stale dangling edge in a WAL must not make
/// startup fail.
#[test]
fn dropped_edges_are_not_logged_and_dangling_edges_do_not_fail_load() {
    use crate::schema::{EdgeType, GraphEdge};
    let dir = tempfile::tempdir().unwrap();
    let snap = dir.path().join("graph.bin");
    let db = GraphDatabase::new(&snap).unwrap();
    db.upsert_node(node("a")).unwrap();
    db.save_to_disk_sync().unwrap();
    let dangling = GraphEdge::new(EdgeType::Calls, node("a").id, "no-such-node".into());
    // One local endpoint: held as a pending cross-repo edge, never added.
    db.insert_edges_batch(std::slice::from_ref(&dangling))
        .unwrap();
    assert_eq!(frames(&snap), 0, "a dropped edge was logged");
    drop(db);

    // A WAL that does contain a dangling edge (written by an older build)
    // must still load.
    append_op(&wal_path_for(&snap), &GraphOp::UpsertEdge(dangling)).unwrap();
    append_op(&wal_path_for(&snap), &GraphOp::UpsertNode(node("b"))).unwrap();
    let p = snap.clone();
    let db = within(15, move || GraphDatabase::new(&p).unwrap());
    assert!(
        db.get_node(&node("b").id).unwrap().is_some(),
        "ops after the dangling edge were skipped"
    );
}

/// CRC-32C check value (RFC 3720 / the standard "123456789" vector).
#[test]
fn crc32c_matches_the_castagnoli_check_value() {
    assert_eq!(crc32c(b"123456789"), 0xE306_9283);
}

/// After a crash tears the WAL tail, recovery must repair the file so frames
/// appended afterwards are reachable by the NEXT recovery.
#[test]
fn ops_logged_after_recovering_a_torn_tail_survive_the_next_crash() {
    let dir = tempfile::tempdir().unwrap();
    let snap = dir.path().join("graph.bin");
    let db = GraphDatabase::new(&snap).unwrap();
    db.upsert_node(node("pre")).unwrap();
    db.save_to_disk_sync().unwrap();
    db.upsert_node(node("a")).unwrap();
    drop(db);
    // Crash mid-append: half a frame of garbage at the tail.
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(wal_path_for(&snap))
            .unwrap();
        f.write_all(&[0x40, 0x00, 0x00, 0x00, 1, 2, 3]).unwrap();
    }
    // Recovery #1, then a new op.
    let p = snap.clone();
    let db = within(15, move || GraphDatabase::new(&p).unwrap());
    db.upsert_node(node("b")).unwrap();
    drop(db);
    // Recovery #2 must see both "a" and "b".
    let p = snap.clone();
    let db = within(15, move || GraphDatabase::new(&p).unwrap());
    for name in ["a", "b"] {
        assert!(
            db.get_node(&node(name).id).unwrap().is_some(),
            "{name} unreachable behind the torn tail"
        );
    }
}

/// Concurrent writers to the same ids: whatever the live graph ends up with
/// must be exactly what replaying the WAL produces.
#[test]
fn wal_order_equals_mutation_order_under_concurrent_writers() {
    let dir = tempfile::tempdir().unwrap();
    let snap = dir.path().join("graph.bin");
    let db = Arc::new(GraphDatabase::new(&snap).unwrap());
    db.upsert_node(node("seed")).unwrap();
    db.save_to_disk_sync().unwrap();

    const IDS: usize = 4;
    let handles: Vec<_> = (0..4)
        .map(|t| {
            let db = Arc::clone(&db);
            std::thread::spawn(move || {
                for i in 0..300 {
                    let mut n = node(&format!("shared{}", i % IDS));
                    n.signature = Some(format!("t{t}-i{i}"));
                    db.upsert_node(n).unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let live: Vec<_> = (0..IDS)
        .map(|k| {
            db.get_node(&node(&format!("shared{k}")).id)
                .unwrap()
                .unwrap()
                .signature
        })
        .collect();
    drop(db);

    let p = snap.clone();
    let recovered = within(30, move || GraphDatabase::new(&p).unwrap());
    let replayed: Vec<_> = (0..IDS)
        .map(|k| {
            recovered
                .get_node(&node(&format!("shared{k}")).id)
                .unwrap()
                .unwrap()
                .signature
        })
        .collect();
    assert_eq!(
        live, replayed,
        "crash recovery diverged from the pre-crash graph"
    );
}

/// Writers keep acknowledging ops while checkpoints run; after a simulated
/// crash (no final save) every acknowledged op must still be recoverable.
#[test]
fn checkpoint_never_loses_an_acknowledged_op() {
    let dir = tempfile::tempdir().unwrap();
    let snap = dir.path().join("graph.bin");
    let db = Arc::new(GraphDatabase::new(&snap).unwrap());
    db.upsert_node(node("seed")).unwrap();
    db.save_to_disk_sync().unwrap();

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let saver = {
        let (db, stop) = (Arc::clone(&db), Arc::clone(&stop));
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                db.save_to_disk_sync().unwrap();
            }
        })
    };
    let mut acked = Vec::new();
    for i in 0..400 {
        let name = format!("n{i}");
        db.upsert_node(node(&name)).unwrap();
        acked.push(name);
    }
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    saver.join().unwrap();
    drop(db); // crash: no final checkpoint

    let p = snap.clone();
    let recovered = within(30, move || GraphDatabase::new(&p).unwrap());
    let missing: Vec<_> = acked
        .iter()
        .filter(|n| recovered.get_node(&node(n).id).unwrap().is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "{} acknowledged ops lost, e.g. {:?}",
        missing.len(),
        &missing[..missing.len().min(5)]
    );
}

/// Two checkpoints at once used to interleave rotate / snapshot / discard: one
/// discarded the retired log the other had just merged live frames into (and on
/// Windows its delete hit the other's open handle: "wal discard: Access is
/// denied", seen in CI). Several savers racing writers must neither error nor
/// lose an acknowledged op.
#[test]
fn concurrent_checkpoints_do_not_error_or_lose_acknowledged_ops() {
    let dir = tempfile::tempdir().unwrap();
    let snap = dir.path().join("graph.bin");
    let db = Arc::new(GraphDatabase::new(&snap).unwrap());
    db.upsert_node(node("seed")).unwrap();
    db.save_to_disk_sync().unwrap();

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let savers: Vec<_> = (0..3)
        .map(|_| {
            let (db, stop) = (Arc::clone(&db), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut n = 0;
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    db.save_to_disk_sync().expect("a checkpoint must not fail");
                    n += 1;
                }
                n
            })
        })
        .collect();
    let mut acked = Vec::new();
    for i in 0..300 {
        let name = format!("c{i}");
        db.upsert_node(node(&name)).unwrap();
        acked.push(name);
    }
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let total: usize = savers.into_iter().map(|h| h.join().unwrap()).sum();
    assert!(total >= 3, "the savers never ran");
    drop(db); // crash: no final checkpoint

    let p = snap.clone();
    let recovered = within(30, move || GraphDatabase::new(&p).unwrap());
    let missing: Vec<_> = acked
        .iter()
        .filter(|n| recovered.get_node(&node(n).id).unwrap().is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "{} acknowledged ops lost, e.g. {:?}",
        missing.len(),
        &missing[..missing.len().min(5)]
    );
}

/// A bulk insert must not pay one open+fsync per node.
#[test]
fn bulk_insert_is_not_one_fsync_per_node() {
    let dir = tempfile::tempdir().unwrap();
    let snap = dir.path().join("graph.bin");
    let db = GraphDatabase::new(&snap).unwrap();
    let nodes: Vec<_> = (0..3000).map(|i| node(&format!("bulk{i}"))).collect();
    let started = std::time::Instant::now();
    db.insert_nodes_batch(&nodes).unwrap();
    let took = started.elapsed();
    assert!(
        took < Duration::from_secs(3),
        "3000-node batch took {took:?}"
    );
}

/// `find_anchors` ranks tied scores deterministically: the same symbols in the
/// same order on every call, in every graph instance (it used to depend on a
/// randomly seeded `HashMap`'s iteration order, so a second `lain oneshot`
/// listed different symbols than the first).
#[test]
fn find_anchors_breaks_score_ties_deterministically() {
    let build = |dir: &Path| {
        let db = GraphDatabase::new(&dir.join("graph.bin")).unwrap();
        for i in (0..40).rev() {
            let mut n = node(&format!("sym{i:02}"));
            n.anchor_score = Some(100.0); // every score tied
            db.upsert_node(n).unwrap();
        }
        db
    };
    let d1 = tempfile::tempdir().unwrap();
    let d2 = tempfile::tempdir().unwrap();
    let names = |db: &GraphDatabase| -> Vec<String> {
        db.find_anchors(10)
            .unwrap()
            .into_iter()
            .map(|n| n.name)
            .collect()
    };
    let (a, b) = (build(d1.path()), build(d2.path()));
    let expected: Vec<String> = (0..10).map(|i| format!("sym{i:02}")).collect();
    assert_eq!(names(&a), expected, "ties must rank by name");
    for _ in 0..20 {
        assert_eq!(names(&a), expected);
        assert_eq!(
            names(&b),
            expected,
            "a second graph instance ranked ties differently"
        );
    }
}

fn some_op(i: usize) -> GraphOp {
    GraphOp::UpsertNode(node(&format!("w{i}")))
}

/// How many `fsync`s each policy issues for 200 appends.
#[test]
fn sync_policy_controls_the_number_of_fsyncs() {
    let dir = tempfile::tempdir().unwrap();
    let count_for = |policy: SyncPolicy, name: &str| {
        let mut w = WalWriter::new(dir.path().join(name), policy);
        for i in 0..200 {
            w.append(&[some_op(i)]).unwrap();
        }
        w.sync_count()
    };
    assert_eq!(count_for(SyncPolicy::Always, "always.wal"), 200);
    assert_eq!(count_for(SyncPolicy::Never, "never.wal"), 0);
    // A long interval: nothing is due during the burst, one explicit sync lands it.
    let mut w = WalWriter::new(
        dir.path().join("interval.wal"),
        SyncPolicy::Interval(Duration::from_secs(3600)),
    );
    for i in 0..200 {
        w.append(&[some_op(i)]).unwrap();
    }
    assert_eq!(
        w.sync_count(),
        0,
        "a burst inside the window must not fsync"
    );
    w.sync().unwrap();
    assert_eq!(w.sync_count(), 1);
    w.sync().unwrap();
    assert_eq!(w.sync_count(), 1, "nothing new to sync");
    assert_eq!(frames_at(&dir.path().join("interval.wal")), 200);
}

fn frames_at(wal: &Path) -> usize {
    replay(wal, |_| Ok(())).unwrap()
}

/// An interval policy does sync once the window has passed.
#[test]
fn interval_policy_syncs_after_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = WalWriter::new(
        dir.path().join("i.wal"),
        SyncPolicy::Interval(Duration::from_millis(400)),
    );
    w.append(&[some_op(0)]).unwrap();
    assert_eq!(w.sync_count(), 0, "inside the window: no sync");
    std::thread::sleep(Duration::from_millis(450));
    w.append(&[some_op(1)]).unwrap();
    assert_eq!(
        w.sync_count(),
        1,
        "the window elapsed, so this append syncs"
    );
}

/// After a rotate the writer must reopen: appends belong in the fresh WAL,
/// not the retired file its old handle points at.
#[test]
fn closing_the_writer_lets_rotate_swap_the_file_under_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.bin.wal");
    let mut w = WalWriter::new(path.clone(), SyncPolicy::Never);
    w.append(&[some_op(0), some_op(1)]).unwrap();
    w.close().unwrap();
    rotate(&path).unwrap();
    w.append(&[some_op(2)]).unwrap();
    w.close().unwrap();
    assert_eq!(
        frames_at(&prev_path_for(&path)),
        2,
        "retired log keeps the old frames"
    );
    assert_eq!(frames_at(&path), 1, "the fresh WAL got only the new frame");
}

#[test]
fn sync_policy_parses_the_environment_forms() {
    // `from_env` reads process state, so exercise the parser through a helper
    // that mirrors it without mutating the environment shared by other tests.
    let parse = |v: &str| match v.trim() {
        "always" => SyncPolicy::Always,
        "never" => SyncPolicy::Never,
        ms => ms
            .parse::<u64>()
            .map(|ms| SyncPolicy::Interval(Duration::from_millis(ms)))
            .unwrap_or(SyncPolicy::Interval(SyncPolicy::DEFAULT_INTERVAL)),
    };
    assert_eq!(parse("always"), SyncPolicy::Always);
    assert_eq!(parse("never"), SyncPolicy::Never);
    assert_eq!(
        parse("250"),
        SyncPolicy::Interval(Duration::from_millis(250))
    );
    assert_eq!(
        parse("garbage"),
        SyncPolicy::Interval(SyncPolicy::DEFAULT_INTERVAL)
    );
}

// ---- replay edge cases found by mutation testing ---------------------------------

fn raw_frame(op: &GraphOp) -> Vec<u8> {
    let mut buf = Vec::new();
    encode_frame(op, &mut buf).unwrap();
    buf
}

/// Only "does not exist" means "no WAL"; any other open/read failure must
/// surface instead of silently reading as an empty log.
#[test]
fn replay_distinguishes_a_missing_wal_from_an_unreadable_one() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        replay(&dir.path().join("absent.wal"), |_| Ok(())).unwrap(),
        0
    );

    // A directory opens but cannot be read: an I/O error, not an empty log.
    let as_dir = dir.path().join("is-a-dir.wal");
    std::fs::create_dir(&as_dir).unwrap();
    assert!(
        replay(&as_dir, |_| Ok(())).is_err(),
        "an unreadable WAL read as empty"
    );
}

/// The frame-size cap is 64 MiB: a header of exactly that size is merely
/// truncated (torn tail), one byte more is corrupt.
#[test]
fn frame_length_cap_boundary() {
    const CAP: u32 = 64 * 1024 * 1024;
    let dir = tempfile::tempdir().unwrap();
    let at_cap = dir.path().join("at.wal");
    std::fs::write(&at_cap, CAP.to_le_bytes()).unwrap();
    assert_eq!(
        replay(&at_cap, |_| Ok(())).unwrap(),
        0,
        "exactly the cap is allowed, then torn"
    );

    let over = dir.path().join("over.wal");
    std::fs::write(&over, (CAP + 1).to_le_bytes()).unwrap();
    assert!(
        replay(&over, |_| Ok(())).is_err(),
        "over the cap is corrupt"
    );
}

/// A realistic large frame (a node with a long docstring) must replay.
#[test]
fn frames_larger_than_a_few_kib_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let wal = dir.path().join("big.wal");
    let mut n = node("big");
    n.docstring = Some("x".repeat(200_000));
    append_op(&wal, &GraphOp::UpsertNode(n.clone())).unwrap();
    let mut seen = 0;
    replay(&wal, |op| {
        if let GraphOp::UpsertNode(got) = op {
            assert_eq!(got.docstring.as_ref().map(|d| d.len()), Some(200_000));
            seen += 1;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, 1);
}

/// Repair truncates to exactly the end of the last intact frame.
#[test]
fn repair_truncates_to_the_last_intact_frame_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let wal = dir.path().join("r.wal");
    let f1 = raw_frame(&GraphOp::UpsertNode(node("a")));
    let f2 = raw_frame(&GraphOp::UpsertNode(node("bb")));
    let mut bytes = [f1.clone(), f2.clone()].concat();
    bytes.extend_from_slice(&[0xde, 0xad, 0xbe]); // torn tail
    std::fs::write(&wal, &bytes).unwrap();

    let n = replay_and_repair(&wal, |_| Ok(())).unwrap();
    assert_eq!(n, 2);
    assert_eq!(
        std::fs::metadata(&wal).unwrap().len() as usize,
        f1.len() + f2.len(),
        "repair must cut exactly at the end of the last good frame"
    );
    // Idempotent: a clean file is left byte-for-byte alone.
    let before = std::fs::read(&wal).unwrap();
    assert_eq!(replay_and_repair(&wal, |_| Ok(())).unwrap(), 2);
    assert_eq!(std::fs::read(&wal).unwrap(), before);
}

/// A bad CRC in the middle stops replay there; repair drops it and everything after.
#[test]
fn repair_cuts_at_a_corrupt_middle_frame() {
    let dir = tempfile::tempdir().unwrap();
    let wal = dir.path().join("m.wal");
    let f1 = raw_frame(&GraphOp::UpsertNode(node("a")));
    let mut f2 = raw_frame(&GraphOp::UpsertNode(node("b")));
    let last = f2.len() - 1;
    f2[last] ^= 0xff; // break the CRC
    let f3 = raw_frame(&GraphOp::UpsertNode(node("c")));
    std::fs::write(&wal, [f1.clone(), f2, f3].concat()).unwrap();
    assert_eq!(replay_and_repair(&wal, |_| Ok(())).unwrap(), 1);
    assert_eq!(std::fs::metadata(&wal).unwrap().len() as usize, f1.len());
}

#[test]
fn discard_prev_is_idempotent_but_reports_real_failures() {
    let dir = tempfile::tempdir().unwrap();
    let wal = dir.path().join("graph.bin.wal");
    discard_prev(&wal).unwrap(); // nothing to discard: fine
    std::fs::write(prev_path_for(&wal), b"x").unwrap();
    discard_prev(&wal).unwrap();
    assert!(!prev_path_for(&wal).exists());
    // A path that cannot be removed as a file is a real error, not "already gone".
    std::fs::create_dir(prev_path_for(&wal)).unwrap();
    assert!(discard_prev(&wal).is_err());
}

/// The first append creates the WAL's directory chain (a fresh `.lain/`).
#[test]
fn append_creates_missing_parent_directories() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a/b/c/graph.wal");
    let mut w = WalWriter::new(path.clone(), SyncPolicy::Always);
    w.append(&[GraphOp::RemoveNodesByIds(vec!["x".into()])])
        .expect("must create parents");
    assert!(path.exists());
}

/// Only "no WAL yet" means "nothing to replay"; any other open failure is an error
/// the caller must see (it would otherwise look like a clean, empty recovery).
#[cfg(unix)]
#[test]
fn replay_reports_open_errors_other_than_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("file");
    std::fs::write(&blocker, "x").unwrap();
    let under_a_file = blocker.join("graph.wal"); // ENOTDIR, not ENOENT
    let r = replay_and_repair(&under_a_file, |_| Ok(()));
    assert!(r.is_err(), "ENOTDIR was reported as an empty WAL: {r:?}");
    assert_eq!(
        replay_and_repair(&dir.path().join("missing.wal"), |_| Ok(())).unwrap(),
        0
    );
}
