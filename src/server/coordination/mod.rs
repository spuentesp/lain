//! Coordination proxy — lain's coordination surface delegates to
//! **zoanthrope** (`@spuentesp/zoanthrope` on npm, `zoanthrope` on
//! PATH).
//!
//! Architecture split (2026-10): zoanthrope owns the coordination core
//! — file claims with exclusive leases, presence, the postboard,
//! advisory/blocking modes (`ZOANTHROPE_MODE`), and content-stamp
//! stale checks. Lain keeps its tool *names* (agents and hooks are
//! wired to them) but every coordination operation is a subprocess
//! call into zoanthrope's stable CLI contract
//! (`zoanthrope claim|release|rejoin|status`, JSON on stdout, exit
//! 0 granted / 1 conflict / 2 usage) or, where the CLI has no mirror
//! (postboard, presence listings), a short-lived `zoanthrope mcp`
//! stdio JSON-RPC round trip. The wire shapes lain's clients already
//! consume (`granted` / `conflicts` with holder naming, `lain_intent`)
//! are produced by the adapters in [`shapes`] — rewrites of the JSON
//! are out of scope here.
//!
//! What stays lain's, and why:
//! - **session auth** ([`crate::server::presence::PresenceRegistry`]):
//!   the `session_token` wrapper around tool calls is an auth concern,
//!   not a coordination concern;
//! - **symbol stamps** ([`symbol_stamps`]): computing a content
//!   fingerprint over declared symbols is code intelligence. The
//!   hashes travel through the proxy as opaque zoanthrope
//!   `caller_stamps` (`name: "symbol:<name>"`, `value: <hash>`) so
//!   zoanthrope stores/returns them and lain can verify drift later;
//! - **graph integration** (`world_state`, overlap detection,
//!   GREEN/YELLOW/RED evaluation): pure lain, fed from proxy reads.
//!
//! Degraded mode: when no `zoanthrope` binary is resolvable
//! (`$ZOANTHROPE_BIN`, then `PATH`), the proxy **fails soft to
//! advisory semantics** — a claim is logged locally with a warning and
//! never blocks, and every JSON response carries
//! `"degraded": "zoanthrope-missing"` so callers can see the workspace
//! is uncoordinated. Lain therefore stays installable standalone.

pub mod proxy;
pub mod shapes;

mod claims;

pub use proxy::{CoordinationProxy, ProxyError};
pub use shapes::{symbols_from_stamps, CallerStamp, ClaimOutcome, Grant, Refusal, DEGRADED_MARKER};

use std::path::{Path, PathBuf};

/// Resolve the `zoanthrope` binary: `$ZOANTHROPE_BIN` first, then
/// `PATH`. Shared by the server-side proxy and the hooks CLI so both
/// degrade identically when coordination is not installed. An
/// explicit `$ZOANTHROPE_BIN` that does not exist is honored as an
/// override — resolution stops there (no PATH fallback), so tests and
/// operators can force degraded mode.
pub fn resolve_zoanthrope_bin() -> Option<PathBuf> {
    if let Ok(bin) = std::env::var("ZOANTHROPE_BIN") {
        let trimmed = bin.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed);
            return p.exists().then_some(p);
        }
    }
    let name = if cfg!(windows) {
        "zoanthrope.exe"
    } else {
        "zoanthrope"
    };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let cmd = dir.join("zoanthrope.cmd");
            if cmd.is_file() {
                return Some(cmd);
            }
        }
    }
    None
}

/// Compute the caller stamps lain pins on every claim that declares
/// symbols: one `{name: "symbol:<name>", value: <blake3-hex>}` pair
/// per declared symbol, where the hash covers that symbol definition's
/// exact source byte range (`SymbolDef::byte_start..byte_end`).
///
/// This is the code-intelligence half of the old claim path (it used
/// to fold the hashes into the in-memory claims registry). The hashes
/// are opaque to zoanthrope — it stores and returns them in
/// `caller_stamps` and verifies only its own file content stamp;
/// verifying the symbol pins is lain's business when it checks for
/// drift later. Symbols the extractor cannot resolve are skipped (a
/// stamp is evidence, never a guess).
pub fn symbol_stamps(path: &Path, symbols: &[String]) -> Vec<CallerStamp> {
    if symbols.is_empty() {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let Ok(src) = std::str::from_utf8(&bytes) else {
        return Vec::new();
    };
    let defs = crate::server::treesitter::extract_definitions(path, src);
    let mut out = Vec::with_capacity(symbols.len());
    for name in symbols {
        let Some(def) = defs.iter().find(|d| d.name == *name) else {
            continue;
        };
        let start = def.byte_start as usize;
        let end = def.byte_end as usize;
        if start > end || end > bytes.len() {
            continue;
        }
        let hash = blake3::hash(&bytes[start..end]);
        out.push(CallerStamp {
            name: format!("symbol:{name}"),
            value: hash.to_hex().to_string(),
        });
    }
    out
}
