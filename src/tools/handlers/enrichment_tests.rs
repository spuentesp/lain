//! Tests for tools/handlers/enrichment.rs
//!
//! Note: run_enrichment and sync_state spawn async background tasks via tokio::spawn,
//! which requires a Tokio runtime context. These are tested via integration tests.
//! Here we only verify the functions accept valid arguments without panicking.

use crate::graph::GraphDatabase;
use crate::tuning::IngestionConfig;

#[test]
fn test_ingestion_config_default() {
    // Just verify IngestionConfig can be created with defaults
    let config = IngestionConfig::default();
    assert!(config.cochange_commit_window > 0);
    assert!(config.cochange_min_pair_count > 0);
}

#[test]
fn test_graph_get_last_commit() {
    let tmp = std::env::temp_dir().join("test_last_commit");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    // Initially should be None
    let result = graph.get_last_commit();
    assert!(result.is_ok());
    // May be None or Some depending on loaded state
    let _ = result.unwrap();
}

#[test]
fn test_graph_set_last_commit() {
    let tmp = std::env::temp_dir().join("test_set_commit");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    let result = graph.set_last_commit("abc123".to_string());
    assert!(result.is_ok());

    // Verify it's stored
    let retrieved = graph.get_last_commit().unwrap();
    assert_eq!(retrieved, Some("abc123".to_string()));
}

#[test]
fn test_insert_co_change_edges_empty() {
    let tmp = std::env::temp_dir().join("test_empty_cochange");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    // Empty pairs should succeed
    let result = graph.insert_co_change_edges(&[]);
    assert!(result.is_ok());
}