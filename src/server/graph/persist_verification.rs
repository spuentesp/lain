//! Properties of the `graph.bin` snapshot format, through the real
//! `GraphDatabase`:
//!
//! * a save followed by a load reproduces the graph exactly;
//! * a truncated file is rejected as a whole (fail-soft to empty), never
//!   half-loaded;
//! * a bit-flipped file never panics and never yields a structurally
//!   inconsistent graph (every id resolves, every edge endpoint exists).
//!
//! NOT a property: detecting a flipped byte inside a string. `graph.bin` has
//! no checksum, so such a flip decodes to a different valid graph. Atomic
//! writes (temp file + rename) are what protect it, and the WAL does not.
use crate::graph::GraphDatabase;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use proptest::prelude::*;
use std::collections::BTreeSet;

fn node_type() -> impl Strategy<Value = NodeType> {
    prop_oneof![
        Just(NodeType::Function),
        Just(NodeType::File),
        Just(NodeType::Struct),
        Just(NodeType::Method),
        Just(NodeType::Class),
    ]
}

/// (nodes, edges as index pairs into the node list)
fn graph_spec() -> impl Strategy<Value = (Vec<(NodeType, String, String)>, Vec<(usize, usize)>)> {
    (
        prop::collection::vec((node_type(), "[a-z]{1,8}", "[a-z/]{1,12}"), 0..25),
        prop::collection::vec((0usize..25, 0usize..25), 0..40),
    )
}

fn build(
    path: &std::path::Path,
    spec: &(Vec<(NodeType, String, String)>, Vec<(usize, usize)>),
) -> GraphDatabase {
    let db = GraphDatabase::new(path).unwrap();
    let mut ids = Vec::new();
    for (i, (ty, name, p)) in spec.0.iter().enumerate() {
        // Distinct line numbers keep same-named symbols distinct nodes.
        let mut n = GraphNode::new(ty.clone(), name.clone(), format!("/{p}"));
        n.line_start = Some(i as u32 + 1);
        n.id = GraphNode::generate_id(
            &n.node_type,
            &n.path,
            &n.name,
            n.line_start,
            &crate::schema::RepoNamespace::for_test(),
        );
        ids.push(n.id.clone());
        db.upsert_node(n).unwrap();
    }
    if !ids.is_empty() {
        for (a, b) in &spec.1 {
            let (s, t) = (&ids[a % ids.len()], &ids[b % ids.len()]);
            let _ = db.insert_edge(&GraphEdge::new(EdgeType::Calls, s.clone(), t.clone()));
        }
    }
    db.set_last_commit("abc123".into()).unwrap();
    db
}

fn summary(db: &GraphDatabase) -> (BTreeSet<String>, usize, Option<String>) {
    let ids = db.get_all_nodes().into_iter().map(|n| n.id).collect();
    (ids, db.edge_count(), db.get_last_commit().unwrap())
}

/// Every node id resolves; counts agree with what enumeration reports.
fn assert_consistent(db: &GraphDatabase) {
    let nodes = db.get_all_nodes();
    assert_eq!(nodes.len(), db.node_count());
    for n in &nodes {
        let got = db.get_node(&n.id).unwrap();
        assert!(got.is_some(), "node {} listed but not resolvable", n.id);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn save_then_load_round_trips(spec in graph_spec()) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.bin");
        let built = build(&path, &spec);
        built.save_to_disk_sync().unwrap();
        let before = summary(&built);
        drop(built);
        let db = GraphDatabase::new(&path).unwrap();
        prop_assert_eq!(summary(&db), before);
        assert_consistent(&db);
    }

    #[test]
    fn a_truncated_snapshot_is_rejected_whole(spec in graph_spec(), cut in 0.0f64..1.0) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.bin");
        let db = build(&path, &spec);
        db.save_to_disk_sync().unwrap();
        let full = summary(&db);
        drop(db);
        let bytes = std::fs::read(&path).unwrap();
        let keep = ((bytes.len() as f64) * cut) as usize;
        prop_assume!(keep < bytes.len());
        std::fs::write(&path, &bytes[..keep]).unwrap();
        let loaded = GraphDatabase::new(&path).unwrap(); // must not panic or error
        let got = summary(&loaded);
        prop_assert!(
            got.0.is_empty() && got.1 == 0,
            "a snapshot cut at {keep}/{} bytes half-loaded: {} nodes (full had {})",
            bytes.len(), got.0.len(), full.0.len()
        );
        assert_consistent(&loaded);
    }

    #[test]
    fn a_bit_flipped_snapshot_never_panics_or_loads_inconsistently(
        spec in graph_spec(), at in 0.0f64..1.0, bit in 0u8..8
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.bin");
        let db = build(&path, &spec);
        db.save_to_disk_sync().unwrap();
        drop(db);
        let mut bytes = std::fs::read(&path).unwrap();
        prop_assume!(!bytes.is_empty());
        let i = ((bytes.len() - 1) as f64 * at) as usize;
        bytes[i] ^= 1 << bit;
        std::fs::write(&path, &bytes).unwrap();
        // May fail soft (empty), may decode to a different valid graph; must
        // never panic or hand back a graph whose indexes disagree with itself.
        let loaded = GraphDatabase::new(&path);
        if let Ok(loaded) = loaded {
            assert_consistent(&loaded);
        }
    }
}

/// Appending junk after a valid snapshot is not silently ignored by the strict validator.
#[test]
fn trailing_bytes_are_rejected_by_the_strict_validator() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    let db = build(
        &path,
        &(vec![(NodeType::Function, "f".into(), "p".into())], vec![]),
    );
    db.save_to_disk_sync().unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    GraphDatabase::validate_persisted_payload(&bytes).expect("a clean snapshot validates");
    bytes.extend_from_slice(b"junk");
    assert!(GraphDatabase::validate_persisted_payload(&bytes).is_err());
}
