//! Lain
//!
//! A structural memory and code intelligence engine for AI agents that provides:
//! - Graph-based code relationships via Petgraph
//! - Semantic search via local NLP embeddings
//! - Real-time Git state tracking
//! - Multi-language LSP support
//! - MCP protocol server

pub mod error;
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

pub use error::LainError;
pub use mcp::LainMcpServer;
pub use server::LainServer;
