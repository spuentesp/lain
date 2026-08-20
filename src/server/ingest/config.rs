//! Server configuration + MCP transport enum shared across the
//! constructors and the LainServer impl block. Split out of
//! `mod.rs` so the constructor helpers (`constructors.rs`) and the
//! 500-line `LainServer` impl (`server.rs`) don't have to live in the
//! same file as the small data types they consume.

use std::path::PathBuf;

/// Per-process configuration held by every `LainServer` instance.
#[derive(Clone)]
pub struct LainConfig {
    /// Data-anchor directory. In single-workspace mode (built via
    /// `LainServer::new`) this is the user's actual workspace — what
    /// `tuning.toml` / `.git/` / `.lsp/` / the tool executor's
    /// workspace all point at. In federation mode (built via
    /// `with_federation*`) this is the placeholder staging dir at
    /// `/tmp/lain-federation-{pid}-{counter}` — federation tools
    /// don't read it (they go through the `FederatedIndex` handle),
    /// per-repo structural tools are bound to the single-repo's real
    /// graph via the binding fix, and git/LSP are placeholders too.
    ///
    /// The state-file stem (used by `state_path()`) is *not* derived
    /// from this field in federation mode — `repos_yaml` is preferred
    /// so restarts pick up the same state. See `state_path()`.
    pub workspace: PathBuf,
    /// Path to `<workspace>/.lain/graph.bin` — the sled database
    /// backing `ctx.graph`. Always `<workspace>/.lain/graph.bin` in
    /// both single-workspace and federation modes; `workspace` is
    /// the staging dir in federation mode (so this is the staging
    /// dir's graph path, which the placeholder executor opens).
    pub memory_path: PathBuf,
}

/// MCP transport for federation-mode servers. Stdio for local agents,
/// Http for network-reachable deployments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Stdio,
    Http,
}

/// Capacity of the `PresenceEvent` broadcast bus. Generous for an
/// interactive session; if a slow consumer falls behind, `send`
/// returns Err and the event is dropped (the registry/occupancy
/// state itself remains consistent on the server side).
pub const PRESENCE_EVENT_CHANNEL_CAPACITY: usize = 256;
