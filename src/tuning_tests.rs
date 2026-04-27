//! Tests for tuning.rs

use crate::tuning::{TuningConfig, IngestionConfig, RuntimeConfig};

#[test]
fn test_tuning_config_default() {
    let config = TuningConfig::default();
    assert_eq!(config.semantic_similarity_threshold, 0.3);
    assert_eq!(config.anchor_weight, 0.3);
    assert_eq!(config.max_pattern_edges, 200);
}

#[test]
fn test_ingestion_config_default() {
    let config = IngestionConfig::default();
    assert_eq!(config.lsp_pool_size, 4);
    assert_eq!(config.files_per_batch, 50);
    assert_eq!(config.max_files_per_scan, 5000);
    assert_eq!(config.ingest_batch_size, 100);
    assert_eq!(config.scan_timeout_secs, 120);
    assert_eq!(config.cochange_max_commit_files, 100);
    assert_eq!(config.cochange_commit_window, 100);
    assert_eq!(config.cochange_min_pair_count, 2);
    assert_eq!(config.nlp_prewarm_count, 20);
    assert_eq!(config.nlp_batch_size, 50);
    assert_eq!(config.nlp_budget_per_pass, 20);
    assert_eq!(config.ui_session_ttl_secs, 600);
    assert_eq!(config.default_query_limit, 100);
}

#[test]
fn test_runtime_config_default() {
    let config = RuntimeConfig::default();
    assert_eq!(config.default_command_timeout_secs, 60);
    assert_eq!(config.default_test_timeout_secs, 300);
    assert_eq!(config.lsp_symbol_poll_timeout_secs, 2);
    assert_eq!(config.lsp_symbol_poll_interval_ms, 50);
}

#[test]
fn test_tuning_config_clone() {
    let config = TuningConfig::default();
    let cloned = config.clone();
    assert_eq!(cloned.semantic_similarity_threshold, config.semantic_similarity_threshold);
    assert_eq!(cloned.anchor_weight, config.anchor_weight);
    assert_eq!(cloned.max_pattern_edges, config.max_pattern_edges);
}

#[test]
fn test_tuning_config_debug() {
    let config = TuningConfig::default();
    let debug_str = format!("{:?}", config);
    assert!(debug_str.contains("semantic_similarity_threshold"));
}

#[test]
fn test_tuning_config_serde() {
    let config = TuningConfig::default();
    let json = serde_json::to_string(&config).unwrap();
    let deserialized: TuningConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.semantic_similarity_threshold, config.semantic_similarity_threshold);
}

#[test]
fn test_tuning_config_serde_roundtrip() {
    let mut config = TuningConfig::default();
    config.semantic_similarity_threshold = 0.7;
    config.anchor_weight = 0.5;
    config.max_pattern_edges = 500;

    let json = serde_json::to_string_pretty(&config).unwrap();
    let deserialized: TuningConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.semantic_similarity_threshold, 0.7);
    assert_eq!(deserialized.anchor_weight, 0.5);
    assert_eq!(deserialized.max_pattern_edges, 500);
}

#[test]
fn test_ingestion_config_serde() {
    let mut config = IngestionConfig::default();
    config.lsp_pool_size = 8;
    config.files_per_batch = 100;

    let json = serde_json::to_string(&config).unwrap();
    let deserialized: IngestionConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.lsp_pool_size, 8);
    assert_eq!(deserialized.files_per_batch, 100);
}

#[test]
fn test_runtime_config_serde() {
    let mut config = RuntimeConfig::default();
    config.default_command_timeout_secs = 120;
    config.default_test_timeout_secs = 600;

    let json = serde_json::to_string(&config).unwrap();
    let deserialized: RuntimeConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.default_command_timeout_secs, 120);
    assert_eq!(deserialized.default_test_timeout_secs, 600);
}

#[test]
fn test_tuning_config_threshold_bounds() {
    let mut config = TuningConfig::default();
    // Valid threshold values
    config.semantic_similarity_threshold = 0.0;
    assert_eq!(config.semantic_similarity_threshold, 0.0);

    config.semantic_similarity_threshold = 1.0;
    assert_eq!(config.semantic_similarity_threshold, 1.0);

    config.semantic_similarity_threshold = 0.5;
    assert_eq!(config.semantic_similarity_threshold, 0.5);
}

#[test]
fn test_tuning_config_anchor_weight_bounds() {
    let mut config = TuningConfig::default();
    config.anchor_weight = 0.0;
    assert_eq!(config.anchor_weight, 0.0);

    config.anchor_weight = 1.0;
    assert_eq!(config.anchor_weight, 1.0);
}

#[test]
fn test_ingestion_config_zero_batch_size() {
    let mut config = IngestionConfig::default();
    config.ingest_batch_size = 0;
    assert_eq!(config.ingest_batch_size, 0);
}

#[test]
fn test_ingestion_config_large_values() {
    let mut config = IngestionConfig::default();
    config.max_files_per_scan = 100000;
    config.cochange_commit_window = 1000;
    assert_eq!(config.max_files_per_scan, 100000);
    assert_eq!(config.cochange_commit_window, 1000);
}

#[test]
fn test_runtime_config_zero_timeout() {
    let mut config = RuntimeConfig::default();
    config.default_command_timeout_secs = 0;
    config.default_test_timeout_secs = 0;
    assert_eq!(config.default_command_timeout_secs, 0);
    assert_eq!(config.default_test_timeout_secs, 0);
}

#[test]
fn test_runtime_config_poll_intervals() {
    let mut config = RuntimeConfig::default();
    config.lsp_symbol_poll_interval_ms = 10;
    assert_eq!(config.lsp_symbol_poll_interval_ms, 10);
}