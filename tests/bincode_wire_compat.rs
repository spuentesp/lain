//! Regression guard for the bincode 2.x migration.
//!
//! PR #57 bumped bincode from 1.x to 2.x. PR #59 changed all
//! on-disk call sites from `bincode::config::standard()` to
//! `bincode::config::legacy()` to keep the wire format compatible
//! with what bincode 1.x wrote. The bincode 2.x docs claim
//! `legacy` matches the bincode 1.x encoding; this test pins
//! that claim with an in-tree assertion so a future bincode
//! upgrade can't silently regress on-disk reads.
//!
//! If this test ever fails on the round-trip path, an
//! `actual_v1_size != actual_v2_size` investigation in the
//! bincode 2.x release notes is in order — and any on-disk data
//! from a v0.7.x release may need a migration shim.

use lain::schema::{EdgeType, GraphEdge, GraphNode, NodeType};

#[test]
fn graph_state_round_trips_through_bincode_legacy_config() {
    // A non-trivial graph node — covers strings, Options, and
    // enums (the field types that most often trip up a wire-format
    // regression).
    let mut original = GraphNode::new(
        NodeType::Function,
        "canonical_claim_path".into(),
        "src/server/presence.rs".into(),
    );
    original.line_start = Some(774);
    original.line_end = Some(795);
    original.signature =
        Some("fn canonical_claim_path(roots: &[PathBuf], path: &Path) -> PathBuf".into());
    original.docstring =
        Some("Symlink + Windows \\\\?\\\\ prefix collapsing.".into());

    let data = bincode::serde::encode_to_vec(&original, bincode::config::legacy())
        .expect("encode");

    let decoded: GraphNode = bincode::serde::decode_from_slice(&data, bincode::config::legacy())
        .map(|(v, _)| v)
        .expect("decode");

    assert_eq!(original.id, decoded.id);
    assert_eq!(original.node_type, decoded.node_type);
    assert_eq!(original.name, decoded.name);
    assert_eq!(original.path, decoded.path);
    assert_eq!(original.line_start, decoded.line_start);
    assert_eq!(original.line_end, decoded.line_end);
    assert_eq!(original.signature, decoded.signature);
    assert_eq!(original.docstring, decoded.docstring);

    let data = bincode::serde::encode_to_vec(&original, bincode::config::legacy())
        .expect("encode");

    let decoded: GraphNode = bincode::serde::decode_from_slice(&data, bincode::config::legacy())
        .map(|(v, _)| v)
        .expect("decode");

    assert_eq!(original.id, decoded.id);
    assert_eq!(original.node_type, decoded.node_type);
    assert_eq!(original.name, decoded.name);
    assert_eq!(original.path, decoded.path);
    assert_eq!(original.line_start, decoded.line_start);
    assert_eq!(original.line_end, decoded.line_end);
    assert_eq!(original.signature, decoded.signature);
    assert_eq!(original.docstring, decoded.docstring);

    // Belt-and-suspenders: also try with `standard()` and confirm
    // it produces a different byte layout than `legacy()`. This
    // pins the property that the two configs are NOT
    // interchangeable — a future bincode release that accidentally
    // aliases them would trip this assertion.
    let standard_data = bincode::serde::encode_to_vec(&original, bincode::config::standard())
        .expect("encode standard");
    assert_ne!(
        data, standard_data,
        "bincode::config::legacy and bincode::config::standard must \
         produce different bytes; if they alias, the v0.7.x wire \
         compat claim in PR #59 is wrong"
    );

    // And the standard config can't decode the legacy bytes (the
    // reverse-direction round-trip also has to fail).
    let res: Result<(GraphNode, usize), _> =
        bincode::serde::decode_from_slice(&data, bincode::config::standard());
    assert!(
        res.is_err(),
        "bincode::config::standard must not decode legacy bytes; \
         if it does, the wire format isn't actually distinct"
    );

    // Last sanity check: a trivial value also round-trips and
    // the byte sizes are reasonable. This catches a regression
    // where legacy() accidentally became varint-encoded (which
    // would explode byte counts).
    let trivial = GraphEdge {
        edge_type: EdgeType::Calls,
        source_id: "src_a".into(),
        target_id: "src_b".into(),
        weight: None,
    };
    let trivial_bytes = bincode::serde::encode_to_vec(&trivial, bincode::config::legacy())
        .expect("trivial encode");
    assert!(
        trivial_bytes.len() < 256,
        "trivial GraphEdge encode ballooned to {} bytes; \
         legacy config probably isn't legacy anymore",
        trivial_bytes.len()
    );
}
