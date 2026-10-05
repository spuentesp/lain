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
