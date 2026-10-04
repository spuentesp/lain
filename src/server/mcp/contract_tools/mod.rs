//! Contract-federation MCP tools (`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §10.1,
//! §10.2, §10.5, §10.9, §12).
//!
//! PR 16 ships the first two: `list_services` and `get_service`. The
//! rest (`prepare_snapshot`, `get_snapshot`, `list_contracts`,
//! `get_contract`, `list_unresolved`, `check_binding`, `diff_contracts`,
//! `trace_impact`, `get_coverage`, `resolve_evidence`, `read_source`)
//! land with PR 11/13. Each tool lives in its own file under this
//! module (one per group); the inventory registration is in `services.rs`
//! for PR 16.
//!
//! ## Inventory pattern (§10.1)
//!
//! `ContractToolEntry` is an inventory-collected `struct` that pairs a
//! tool name with an async handler (`BoxFuture<'static, _>`). The
//! dispatcher in `mcp::handler::dispatch_tool_call` checks the
//! inventory after `invoke_inventory` (right after the `match` arm
//! that resolves the legacy `McpToolEntry` path) and dispatches if a
//! contract tool matches. **No new match arm** is added to
//! `dispatch_tool_call` — the `scripts/check-mcp-dispatch-shape.py`
//! guard stays clean.
//!
//! ## Snapshot gating (§10.1 / §10.5)
//!
//! For PR 16 the only valid snapshot is `"live"`. Anything else
//! returns `snapshot_not_found` with no federation read. PR 11/13
//! surfaces the snapshot manager (`fed.snapshots()`) and the
//! `ContractIndex` will then be looked up from a named snapshot
//! instead.
//!
//! ## Envelope (§10.2)
//!
//! Every successful response wraps the tool's `data` in an
//! `Envelope<T>` with `api_version`, `analyzer_version`, `snapshot`,
//! `reproducible` (false on `live`), and `meta.elapsed_ms`. The plain-
//! text rendering is `cap_2000`d, with the §9.6 scope sentence
//! appended whenever `scope` is present. Errors set `is_error: true`
//! and return a `ToolError`-shaped JSON (`code`, `message`,
//! `retryable`, optional `details`).
//!
//! ## Rejoin (§10.1 / §5.3)
//!
//! Every tool calls `fed.rejoin_contracts_if_dirty()` before reading
//! the contract index. The dirty flag is set by `project_nodes`,
//! `project_edges`, hot-reload, `add_repo`, `remove_repo`, and the
//! refresh-loop tick; `rejoin_contracts_if_dirty` is a no-op when the
//! flag is clear.

pub mod analysis;
pub mod contracts;
pub mod envelope;
pub mod evidence;
pub mod paging;
pub mod scope;
pub mod services;
pub mod snapshots;
pub mod used_by;

use crate::server::mcp::handler::McpContext;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;

/// The result of a contract tool call. `structured` is the JSON the
/// client reads from `structuredContent`; `text` is the
/// `content[0].text` rendering (≤ 2,000 chars). `is_error: true`
/// means the envelope is an error envelope (`api_version`,
/// `analyzer_version`, `error: ToolError`).
#[derive(Debug)]
pub struct ToolOutcome {
    pub structured: Value,
    pub text: String,
    pub is_error: bool,
}

/// Inventory entry for a contract tool (`§10.1`). The handler is a
/// free-function pointer so the inventory collection works at static-
/// init time; the handler returns a boxed future so `wait_ms` long-
/// polls don't block a runtime thread. `'static` lifetime avoids the
/// `Send` bound; the dispatcher awaits inside its own `tokio`
/// single-thread task.
///
/// Handler trampoline:
///
/// ```ignore
/// pub fn handle(ctx: &McpContext, args: Value) -> BoxFuture<'static, Result<ToolOutcome, String>> {
///     Box::pin(async move {
///         // ... call into services.rs ...
///         Ok(ToolOutcome { structured, text, is_error: false })
///     })
/// }
/// ```
/// Type alias for the boxed-future return type of a contract tool
/// handler. `Pin<Box<dyn Future + Send>>` so the future composes with
/// the async dispatcher (whose callers require `Send`). The lifetime
/// `'a` is bound to the `&McpContext` so the handler can borrow
/// `ctx` without crossing thread boundaries.
pub type ContractToolFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ToolOutcome, String>> + Send + 'a>>;

/// The handler signature: `fn(&McpContext, Value) -> ContractToolFuture`.
pub type ContractToolHandler = for<'a> fn(&'a McpContext<'a>, Value) -> ContractToolFuture<'a>;

pub struct ContractToolEntry {
    pub name: &'static str,
    pub handler: ContractToolHandler,
}

inventory::collect!(ContractToolEntry);

/// One API version is served: `1`. The contract tools refuse any
/// request that asks for `api_version != 1` with
/// `unsupported_api_version` (`details.supported: [1]`).
pub const API_VERSION: u32 = 1;

/// The analyzer version reported on every envelope. Tracking the
/// `Cargo.toml` version is the right answer until the contract
/// analyzers land their own version (`§10.3`); at that point this
/// constant becomes a build-script-injected string.
pub const ANALYZER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default limit and max for `list_services` and the consumer paging
/// of `get_service` (`§10.5`).
pub const DEFAULT_LIMIT: usize = 100;
pub const MAX_LIMIT: usize = 1000;

/// `get_service.depth` defaults and cap (`§10.5`).
pub const DEFAULT_DEPTH: u8 = 4;
pub const MAX_DEPTH: u8 = 8;

/// Hard cap on the rendered text (`§10.2`).
pub const TEXT_CAP_CHARS: usize = 2000;
