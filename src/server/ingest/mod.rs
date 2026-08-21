//! Ingest pipeline + Lain server orchestration.
//!
//! `LainServer` wires together every component the MCP layer needs: the
//! persistent graph, the volatile overlay, the embedder + cross-encoder,
//! the git sensor, the LSP pool, and the tool executor. It owns the
//! federation handle (set by `with_federation*`) and the background-sync
//! job. The sibling modules carry the actual work:
//!
//! - [`config`] — `LainConfig`, `Transport`, `PRESENCE_EVENT_CHANNEL_CAPACITY`.
//! - [`constructors`] — `LainServer::new`, the four `with_federation*`
//!   variants, and the private `build_federation_server` they delegate to.
//! - [`server`] — the `LainServer` struct + every non-constructor method
//!   (accessors, `add_repo`, `remove_repo`, `serve`, persistence).
//! - [`background`] — the presence expiry loop and the attribution
//!   watcher spawned by the constructors.
//! - [`ingestion`] — the single-workspace ingest pipeline
//!   (`build_core_memory`) and the federation ingestion entry point
//!   (`index_one_repo`).
//! - [`resolve`] — the three resolve phases shared by both pipelines.
//! - [`scan`] — per-file tree-sitter extraction.
//! - [`jobs`] — the background enrichment/co-change jobs.

pub mod background;
pub mod config;
pub mod constructors;
pub mod ingestion;
pub mod jobs;
pub mod resolve;
pub mod scan;
pub mod server;

pub use config::{LainConfig, Transport, PRESENCE_EVENT_CHANNEL_CAPACITY};
pub use server::LainServer;

/// Per-process counter that disambiguates the staging dir used by
/// `LainServer::with_federation`. The placeholder `LainServer` builds a
/// throwaway git repo at `/tmp/lain-federation-{pid}-{counter}` so
/// parallel tests in the same process don't race on a shared path.
pub(crate) static STAGING_COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
