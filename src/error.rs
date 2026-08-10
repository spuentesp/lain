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
    Io(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Unsupported manifest version: {0}")]
    UnsupportedManifestVersion(u32),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Unavailable: {0}")]
    Unavailable(String),

    #[error("Invalid repo id: {0}")]
    InvalidRepoId(String),

    #[error("Invalid global id: {0}")]
    InvalidGlobalId(String),

    #[error("Fatal: {0}")]
    Fatal(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("Workspace error: {0}")]
    Workspace(String),

    #[error("Not implemented: {0}")]
    NotImplemented(String),

    #[error("Other error: {0}")]
    Other(String),

    #[error("Ambiguous symbol: matches repos {0:?}")]
    AmbiguousSymbol(Vec<crate::federation::repo_id::RepoId>),
}

impl From<git2::Error> for LainError {
    fn from(err: git2::Error) -> Self {
        LainError::Git(err.message().to_string())
    }
}

impl From<std::io::Error> for LainError {
    fn from(err: std::io::Error) -> Self {
        LainError::Io(err.to_string())
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
