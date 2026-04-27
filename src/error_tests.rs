//! Tests for error.rs

use crate::error::LainError;
use std::io;

#[test]
fn test_lain_error_git() {
    let err = LainError::Git("ref not found".to_string());
    assert_eq!(format!("{}", err), "Git error: ref not found");
}

#[test]
fn test_lain_error_graph() {
    let err = LainError::Graph("node not found".to_string());
    assert_eq!(format!("{}", err), "Graph database error: node not found");
}

#[test]
fn test_lain_error_database() {
    let err = LainError::Database("connection refused".to_string());
    assert_eq!(format!("{}", err), "Database error: connection refused");
}

#[test]
fn test_lain_error_lsp() {
    let err = LainError::Lsp("timeout".to_string());
    assert_eq!(format!("{}", err), "LSP error: timeout");
}

#[test]
fn test_lain_error_nlp() {
    let err = LainError::Nlp("model load failed".to_string());
    assert_eq!(format!("{}", err), "NLP error: model load failed");
}

#[test]
fn test_lain_error_mcp() {
    let err = LainError::Mcp("invalid request".to_string());
    assert_eq!(format!("{}", err), "MCP error: invalid request");
}

#[test]
fn test_lain_error_io() {
    let io_err = io::Error::new(io::ErrorKind::NotFound, "file not found");
    let err = LainError::from(io_err);
    assert_eq!(format!("{}", err), "IO error: file not found");
}

#[test]
fn test_lain_error_json() {
    let json_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
    let err = LainError::from(json_err);
    assert!(format!("{}", err).contains("JSON error"));
}

#[test]
fn test_lain_error_not_found() {
    let err = LainError::NotFound("symbol foo not found".to_string());
    assert_eq!(format!("{}", err), "Not found: symbol foo not found");
}

#[test]
fn test_lain_error_unavailable() {
    let err = LainError::Unavailable("server not running".to_string());
    assert_eq!(format!("{}", err), "Unavailable: server not running");
}

#[test]
fn test_lain_error_fatal() {
    let err = LainError::Fatal("unrecoverable state".to_string());
    assert_eq!(format!("{}", err), "Fatal: unrecoverable state");
}

#[test]
fn test_lain_error_debug() {
    let err = LainError::Git("test".to_string());
    let debug_str = format!("{:?}", err);
    assert!(debug_str.contains("Git"));
}

#[test]
fn test_lain_error_serialize() {
    let err = LainError::NotFound("test error".to_string());
    let json = serde_json::to_string(&err).unwrap();
    assert!(json.contains("Not found: test error"));
}

#[test]
fn test_lain_error_from_git2() {
    let git_err = git2::Error::new(git2::ErrorCode::NotFound, git2::ErrorClass::Reference, "reference not found");
    let err = LainError::from(git_err);
    assert!(format!("{}", err).contains("reference not found"));
}

#[test]
fn test_lain_error_all_variants() {
    let variants = [
        LainError::Git("g".to_string()),
        LainError::Graph("g".to_string()),
        LainError::Database("d".to_string()),
        LainError::Lsp("l".to_string()),
        LainError::Nlp("n".to_string()),
        LainError::Mcp("m".to_string()),
        LainError::NotFound("n".to_string()),
        LainError::Unavailable("u".to_string()),
        LainError::Fatal("f".to_string()),
    ];

    for err in variants {
        let json = serde_json::to_string(&err);
        assert!(json.is_ok());
    }
}