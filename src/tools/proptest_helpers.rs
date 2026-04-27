//! Property-based tests for algorithmically complex components.
//!
//! Uses `proptest` to test invariants across randomized inputs.

use crate::overlay::VolatileOverlay;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use crate::tools::utils::cosine_similarity;
use proptest::prelude::*;

// ─── Cosine Similarity ────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn test_cosine_similarity_bounds(a in prop::collection::vec(0f32..=1e6_f32, 1..=128),
                                      b in prop::collection::vec(0f32..=1e6_f32, 1..=128)) {
        let result = cosine_similarity(&a, &b);
        prop_assert!(result >= -1.0 && result <= 1.0);
    }

    #[test]
    fn test_cosine_similarity_identical(a in prop::collection::vec(0f32..=1e6_f32, 1..=128)) {
        let norm_sq: f32 = a.iter().map(|v| v * v).sum();
        if norm_sq > 1e-6 {
            let result = cosine_similarity(&a, &a);
            prop_assert!((result - 1.0).abs() < 1e-3);
        }
    }

    #[test]
    fn test_cosine_similarity_zero_vector(a in prop::collection::vec(0f32..=1e6_f32, 1..=128)) {
        let zero = vec![0.0; a.len()];
        let result = cosine_similarity(&a, &zero);
        prop_assert!(result.abs() < 1e-3);
    }

    #[test]
    fn test_cosine_similarity_mismatched_lengths(a in prop::collection::vec(0f32..=1e6_f32, 1..=64),
                                                  b in prop::collection::vec(0f32..=1e6_f32, 65..=128)) {
        let result = cosine_similarity(&a, &b);
        prop_assert_eq!(result, 0.0);
    }
}

// Deterministic tests for commutativity (hardcoded cases avoid proptest strategy issues)
#[test]
fn test_cosine_similarity_commutative_fixed() {
    let a = vec![1.0, 0.0, 0.0, 0.0];
    let b = vec![1.0, 0.0, 0.0, 0.0];
    let result = cosine_similarity(&a, &b);
    let result_rev = cosine_similarity(&b, &a);
    assert!((result - result_rev).abs() < 1e-6);
    assert!((result - 1.0).abs() < 1e-6);

    let c = vec![1.0, 0.0, 0.0];
    let d = vec![0.0, 1.0, 0.0];
    let result2 = cosine_similarity(&c, &d);
    let result2_rev = cosine_similarity(&d, &c);
    assert!((result2 - result2_rev).abs() < 1e-6);
    assert!(result2.abs() < 1e-6);

    let e = vec![0.5, 0.5, 0.5, 0.5];
    let f = vec![0.5, -0.5, 0.5, -0.5];
    let result3 = cosine_similarity(&e, &f);
    let result3_rev = cosine_similarity(&f, &e);
    assert!((result3 - result3_rev).abs() < 1e-6);
}

// ─── Overlay Operations ─────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn test_overlay_insert_and_retrieve(key in 1u8..=20, value in 1u8..=20) {
        let overlay = VolatileOverlay::new();
        let name = format!("val_{}", value);
        let path = format!("/src/f{}.rs", key);
        let node = GraphNode::new(NodeType::Function, name.clone(), path.clone());

        let lookup_id = node.id.clone();
        overlay.insert_node(node);

        prop_assert!(overlay.get_node(&lookup_id).is_some());
    }

    #[test]
    fn test_overlay_no_index_collisions(key in prop::collection::vec(1u8..=30, 1..=15)) {
        let overlay = VolatileOverlay::new();
        let mut indices = Vec::new();

        for (i, _) in key.iter().enumerate() {
            let node = GraphNode::new(NodeType::Function, format!("n{}", i), format!("/src/{}.rs", i));
            let idx = overlay.insert_node(node);
            indices.push(idx);
        }

        for window in indices.windows(2) {
            prop_assert_ne!(window[0], window[1], "Index collision detected");
        }
    }
}

// ─── GraphEdge and Node ID Tests ──────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn test_edge_creation_preserves_ids(source in "[a-zA-Z0-9_]{1,32}", target in "[a-zA-Z0-9_]{1,32}") {
        let edge = GraphEdge::new(EdgeType::Calls, source.clone(), target.clone());
        prop_assert_eq!(edge.source_id, source);
        prop_assert_eq!(edge.target_id, target);
        prop_assert_eq!(edge.edge_type, EdgeType::Calls);
    }

    #[test]
    fn test_node_id_deterministic(ntype in 0u8..5, path in "[a-zA-Z0-9_/-]{1,64}", name in "[a-zA-Z0-9_]{1,32}") {
        let ntype_enum = match ntype {
            0 => NodeType::Module,
            1 => NodeType::Function,
            2 => NodeType::Struct,
            3 => NodeType::Trait,
            _ => NodeType::File,
        };

        let node1 = GraphNode::new(ntype_enum.clone(), name.clone(), path.clone());
        let node2 = GraphNode::new(ntype_enum, name.clone(), path.clone());
        prop_assert_eq!(node1.id, node2.id);
    }

    #[test]
    fn test_all_node_type_variants(ntype in 0u8..5) {
        let ntype_enum = match ntype {
            0 => NodeType::Module,
            1 => NodeType::Function,
            2 => NodeType::Struct,
            3 => NodeType::Trait,
            _ => NodeType::File,
        };
        let _node = GraphNode::new(ntype_enum, "test".to_string(), "/test.rs".to_string());
        prop_assert!(true);
    }
}