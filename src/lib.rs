//! Lain
//!
//! A structural memory and code intelligence engine for AI agents that provides:
//! - Graph-based code relationships via Petgraph
//! - Semantic search via local NLP embeddings
//! - Real-time Git state tracking
//! - Multi-language LSP support
//! - MCP protocol server

pub mod error;
pub mod federation;
pub mod graph;
pub mod git;
pub mod lsp;
pub mod mcp;
pub mod nlp;
pub mod overlay;
pub mod query;
pub mod schema;
pub mod server;
pub mod tools;
pub mod treesitter;
pub mod sensors;
pub mod toolchains;
pub mod tuning;
pub mod watcher;

#[cfg(test)]
mod overlay_tests;
#[cfg(test)]
mod git_tests;
#[cfg(test)]
mod error_tests;
#[cfg(test)]
mod schema_tests;
#[cfg(test)]
mod tuning_tests;
#[cfg(test)]
mod graph_tests;
#[cfg(test)]
#[path = "federation/repo_source_tests.rs"]
mod repo_source_tests;
#[cfg(test)]
#[path = "federation/graph_backend_tests.rs"]
mod graph_backend_tests;
#[cfg(test)]
#[path = "federation/matching_tests.rs"]
mod matching_tests;
#[cfg(test)]
#[path = "federation/federated_index_tests.rs"]
mod federated_index_tests;
#[cfg(test)]
#[path = "federation/manifest_tests.rs"]
mod manifest_tests;
#[cfg(test)]
#[path = "federation/loader_tests.rs"]
mod loader_tests;

pub use error::LainError;
pub use mcp::LainMcpServer;
pub use server::LainServer;
