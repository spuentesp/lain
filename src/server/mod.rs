//! MCP server — the headline of `lain`.
//!
//! Owns the federation engine, the workspace layer, all analytical tools,
//! the ingest pipeline, the watcher, and the volatile overlay.

pub mod federation;
pub mod mcp;
pub mod tools;

// Core
pub mod graph;
pub mod schema;
pub mod git;
pub mod treesitter;
pub mod tuning;
pub mod error;

// Analytical side
pub mod nlp;
pub mod toolchains;
pub mod sensors;
pub mod watcher;
pub mod overlay;
pub mod reload;

pub mod ingest;
