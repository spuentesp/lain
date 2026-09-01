//! Command Center SPA static assets served by the HTTP `/mcp` server.
//! The assets are compiled in with `include_str!` / `include_bytes!`
//! so the binary stays self-contained (no filesystem dependency on
//! the SPA at runtime).
//!
//! Asset list:
//! - GET `/`              → index.html (text/html)
//! - GET `/index.html`    → index.html
//! - GET `/app.js`        → app.js (text/javascript)
//! - GET `/theme.css`     → theme.css (text/css) — shared palette
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

/// The shared 80s-console palette (light + dark). Loaded by the SPA shell
/// and by every standalone `/ui/*` detail view, so one token set themes
/// the whole surface.
pub const THEME_CSS: &[u8] = include_bytes!("command_center/theme.css");

/// The blast-radius detail view (separate from the SPA shell;
/// served at `/ui/blast-radius/{id}` for the interactive picker).
pub const BLAST_RADIUS_HTML: &str = include_str!("../../ui/blast-radius.html");

/// The coupling-radar detail view.
pub const COUPLING_HTML: &str = include_str!("../../ui/coupling.html");

/// The call-chain detail view.
pub const CALL_CHAIN_HTML: &str = include_str!("../../ui/call-chain.html");

// ── dev-mode SPA override (parked-bug #7) ──────────────────────────────
//
// `LAIN_DEV_SPA_DIR=<path>` flips the assets module to read each
// file from disk instead of returning the embedded bytes. Used by
// the JS/CSS dev loop: edit `src/server/mcp/command_center/app.js`,
// refresh the browser, no `cargo build` needed. Production builds
// leave the env var unset; the `dev_spa_dir()` call below is a
// single env-var lookup + is_dir syscall per request — cheap
// enough to not bother caching.

use std::borrow::Cow;
use std::path::PathBuf;

/// Return Some(PathBuf) iff `LAIN_DEV_SPA_DIR` is set AND points at an
/// existing directory. The check is per-call so flipping the env var
/// at runtime takes effect on the next request.
pub fn dev_spa_dir() -> Option<PathBuf> {
    let raw = std::env::var_os("LAIN_DEV_SPA_DIR")?;
    let p = PathBuf::from(raw);
    if p.is_dir() {
        Some(p)
    } else {
        None
    }
}

/// Return the embedded `&'static [u8]` for `name`, unless
/// `LAIN_DEV_SPA_DIR` is set AND `<dir>/<name>` is a readable file —
/// in which case the file's bytes are returned. `name` is relative to
/// the SPA root (e.g. "index.html", "app.js", "styles.css").
pub fn serve_bytes(name: &str, embedded: &'static [u8]) -> Cow<'static, [u8]> {
    if let Some(dir) = dev_spa_dir() {
        let path = dir.join(name);
        if let Ok(bytes) = std::fs::read(&path) {
            return Cow::Owned(bytes);
        }
    }
    Cow::Borrowed(embedded)
}

/// String variant for entries whose embedded form is `&'static str`
/// (the `SPA_ASSETS` table — `d3.v7.min.js` etc.). Same semantics as
/// `serve_bytes`; takes `&'static str` to match the table's type.
pub fn serve_str(name: &str, embedded: &'static str) -> Cow<'static, [u8]> {
    if let Some(dir) = dev_spa_dir() {
        let path = dir.join(name);
        if let Ok(bytes) = std::fs::read(&path) {
            return Cow::Owned(bytes);
        }
    }
    Cow::Borrowed(embedded.as_bytes())
}
