//! MCP server — the headline of `lain`.
//!
//! Owns the federation engine, the workspace layer, all analytical tools,
//! the ingest pipeline, the watcher, and the volatile overlay.

pub mod federation;
pub mod mcp;
pub mod refresh;
pub mod tools;

// Core
pub mod auth;
pub mod build_info;
pub mod events_log;
pub mod graph;
pub mod schema;
pub mod git;
pub mod lsp;
pub mod treesitter;
pub mod tuning;
pub mod error;
pub mod time;

// Analytical side
pub mod nlp;
pub mod toolchains;
pub mod sensors;
pub mod watcher;
pub mod overlay;
pub mod revision_log;
pub mod sync_status;

pub mod ingest;
pub mod query;
pub mod reload;

// Multiplayer awareness
pub mod attribution;
pub mod presence;
pub mod presence_lock;
pub mod sentinel;
pub mod state_lock;
pub mod sse;

// Audit log — append-only JSONL record of every edit that lands
// on disk. See `crate::server::audit` for the storage model and
// rotation semantics.
pub mod audit;

// Tiny glob matcher used by the `get_audit_log` MCP tool (Task 2.5)
// to filter audit events by path. Thin shim over the `glob` crate
// that is already a project dependency.
pub mod glob_match;

// Re-export the LainServer orchestrator + transport at `crate::server::*`
// so existing callers (`lain::server::LainServer`, `lain::server::Transport`,
// `crate::server::LainServer`, `crate::server::Transport`) keep working
// after the body moved into `crate::server::ingest`.
pub use crate::server::ingest::{LainConfig, LainServer, Transport};

// These test modules existed on disk but were never declared, so they had
// never been compiled or run — 1,649 lines of dormant coverage. They pass as
// written; nothing here is a rewrite. Worth noting what they do *not* cover:
// `git_tests.rs` never exercised `get_new_commits_since`, so wiring these up
// earlier would not by itself have caught the inverted revwalk. That gap now
// has its own regression test.
#[cfg(test)]
mod error_tests;
#[cfg(test)]
mod git_tests;
#[cfg(test)]
mod graph_tests;
#[cfg(test)]
mod overlay_tests;
#[cfg(test)]
mod schema_tests;
#[cfg(test)]
mod tuning_tests;
