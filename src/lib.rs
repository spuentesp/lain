//! Lain - Language-Augmented Ingestion Network
//!
//! A structural memory and architectural brain for AI agents that provides:
//! - Graph-based code relationships via KùzuDB
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
pub mod schema;
pub mod server;
pub mod tools;

pub use error::LainError;
pub use server::LainServer;
