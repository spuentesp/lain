//! Lain error types

use thiserror::Error;

/// Main Lain error type
#[derive(Error, Debug)]
pub enum LainError {
    #[error("Git error: {0}")]
    Git(String),

    #[error("Graph database error: {0}")]
    Graph(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("LSP error: {0}")]
    Lsp(String),

    #[error("NLP error: {0}")]
    Nlp(String),

    #[error("MCP error: {0}")]
    Mcp(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Unavailable: {0}")]
    Unavailable(String),

    #[error("Fatal: {0}")]
    Fatal(String),
}

impl From<git2::Error> for LainError {
    fn from(err: git2::Error) -> Self {
        LainError::Git(err.message().to_string())
    }
}

impl<T> From<ort::Error<T>> for LainError {
    fn from(err: ort::Error<T>) -> Self {
        LainError::Nlp(err.to_string())
    }
}

impl serde::Serialize for LainError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}
