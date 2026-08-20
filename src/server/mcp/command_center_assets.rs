//! Command Center SPA static assets served by the HTTP `/mcp` server.
//! The assets are compiled in with `include_str!` / `include_bytes!`
//! so the binary stays self-contained (no filesystem dependency on
//! the SPA at runtime).
//!
//! Asset list:
//! - GET `/`              → index.html (text/html)
//! - GET `/index.html`    → index.html
//! - GET `/app.js`        → app.js (text/javascript)
//! - GET `/styles.css`    → styles.css (text/css)
//! - GET `/assets/*`      → vendored static assets under
//!                           command_center/assets/
//!
//! `/assets/*` is whitelisted explicitly so an unvetted path under
//! that prefix can't be used to exfiltrate other include_str!()
//! targets (defense in depth — the asset list is a fixed set we
//! control, not a filesystem glob).

/// Whitelisted SPA assets under `/assets/`. Each entry is
/// `(route, body)`. New assets are added here by appending a row;
/// the HTTP request handler iterates this list.
pub const SPA_ASSETS: &[(&str, &str)] = &[
    (
        "/assets/d3.v7.min.js",
        include_str!("command_center/assets/d3.v7.min.js"),
    ),
];

pub const INDEX_HTML: &[u8] = include_bytes!("command_center/index.html");
pub const APP_JS: &[u8] = include_bytes!("command_center/app.js");
pub const STYLES_CSS: &[u8] = include_bytes!("command_center/styles.css");

/// The blast-radius detail view (separate from the SPA shell;
/// served at `/ui/blast-radius/{id}` for the interactive picker).
pub const BLAST_RADIUS_HTML: &str = include_str!("../../ui/blast-radius.html");

/// The coupling-radar detail view.
pub const COUPLING_HTML: &str = include_str!("../../ui/coupling.html");

/// The call-chain detail view.
pub const CALL_CHAIN_HTML: &str = include_str!("../../ui/call-chain.html");
