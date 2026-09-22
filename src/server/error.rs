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
    #[cfg_attr(not(feature = "nlp"), allow(dead_code))]
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

    /// Cooperative shutdown observed. Returned by long-running phases
    /// (`build_core_memory`, `index_one_repo`, watchers) when the
    /// server-owned `CancellationToken` is cancelled mid-pass. The
    /// `AwaitStartup`/`background_sync` callers translate this into
    /// `unavailable_error` with code `index_cancelled`.
    #[error("Operation cancelled")]
    Cancelled,

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

#[cfg(feature = "nlp")]
impl<T> From<ort::Error<T>> for LainError {
    fn from(err: ort::Error<T>) -> Self {
        LainError::Nlp(err.to_string())
    }
}

impl From<bincode::error::DecodeError> for LainError {
    fn from(err: bincode::error::DecodeError) -> Self {
        LainError::Serialization(format!("bincode decode: {err}"))
    }
}

impl From<bincode::error::EncodeError> for LainError {
    fn from(err: bincode::error::EncodeError) -> Self {
        LainError::Serialization(format!("bincode encode: {err}"))
    }
}

impl From<serde_yaml::Error> for LainError {
    fn from(err: serde_yaml::Error) -> Self {
        LainError::Serialization(format!("yaml: {err}"))
    }
}

impl From<toml::de::Error> for LainError {
    fn from(err: toml::de::Error) -> Self {
        LainError::Config(format!("toml decode: {err}"))
    }
}

impl From<toml::ser::Error> for LainError {
    fn from(err: toml::ser::Error) -> Self {
        LainError::Config(format!("toml encode: {err}"))
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
