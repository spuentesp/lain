//! `lain doctor` — one-version-of-truth diagnostic for the installation.
//!
//! Wishlist item #6: when an operator (or an automated bug report) needs
//! to know "is this binary the one I think it is, and is the local state
//! sane?", they run `lain doctor` and get a single page that names:
//!
//! 1. The binary version (Cargo.toml) + the git short SHA captured at
//!    build time by `build.rs`.
//! 2. Whether the on-disk hook scripts the agents rely on are present.
//! 3. Whether the config dir (`~/.config/lain`) is present / creatable.
//! 4. Whether the hooks dir (`<config>/hooks`) is present / creatable
//!    and how many cached session JSON files it carries.
//! 5. Whether the in-process presence registry constructs cleanly (i.e.
//!    the data structures the server hot path relies on are reachable).
//! 6. If `LAIN_URL` or `LAIN_SERVER_URL` is set, whether the server is
//!    reachable — this is a soft `[WARN]` rather than a hard `[FAIL]`
//!    because `lain doctor` is also useful in environments where no
//!    server is running locally.
//!
//! Returns `Ok(0)` if every check passed, `Ok(1)` if any hard check
//! failed. Hard failures do not abort early — the operator should see
//! every result, not just the first one that broke.

use crate::config::{config_dir, hooks_dir, lain_git_sha};
use crate::server::presence::PresenceRegistry;
use anyhow::Result;

/// One diagnostic result line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Ok,
    Fail,
    Warn,
}

impl Severity {
    fn tag(self) -> &'static str {
        match self {
            Severity::Ok => "[OK]",
            Severity::Fail => "[FAIL]",
            Severity::Warn => "[WARN]",
        }
    }
}

fn emit(sev: Severity, msg: impl AsRef<str>) -> bool {
    println!("{} {}", sev.tag(), msg.as_ref());
    sev != Severity::Fail
}

/// Run the diagnostic. Returns the exit code (0 = all hard checks OK,
/// 1 = at least one hard check failed). Wrapped in `Result` so the
/// caller can `?`-propagate unexpected panics from `reqwest` etc.
pub fn run_doctor() -> Result<i32> {
    println!("== lain doctor ==\n");

    let mut failures = 0usize;

    // Check 1: binary version + git sha (one-version-of-truth).
    let version = env!("CARGO_PKG_VERSION");
    let sha = lain_git_sha();
    if !emit(
        Severity::Ok,
        format!("binary version : {version} (commit {sha})"),
    ) {
        failures += 1;
    }

    // Check 2: hook scripts present on disk. The Claude Code hook is
    // the reference one — every agent's pre-edit integration points
    // at the same `hooks/<kind>/pre-edit.sh` layout. For dev builds
    // they're at `$CARGO_MANIFEST_DIR/hooks/`; for installed binaries
    // they ship next to the binary itself (the install layout puts
    // them in `$bindir/../share/lain/hooks/`, but a flat `cargo
    // install --path .` lands them next to the executable). Check
    // both, plus the legacy single-file dev path, before failing —
    // a `[FAIL]` here on a release binary was a long-standing bug
    // (wishlist #6's "one version of truth" promise).
    let source_hook = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("hooks/claude-code/pre-edit.sh");
    let installed_hook = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join("../share/lain/hooks/claude-code/pre-edit.sh")))
        .unwrap_or_else(|| std::path::PathBuf::from("<no install path>"));
    let flat_hook = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join("hooks/claude-code/pre-edit.sh")))
        .unwrap_or_else(|| std::path::PathBuf::from("<no flat path>"));
    let candidates = [&source_hook, &installed_hook, &flat_hook];
    let found = candidates.iter().find(|p| p.exists());
    if let Some(p) = found {
        if !emit(
            Severity::Ok,
            format!("claude-code hook script present: {}", p.display()),
        ) {
            failures += 1;
        }
    } else {
        emit(
            Severity::Fail,
            format!(
                "claude-code hook script MISSING. Looked at: {} ; {} ; {}",
                source_hook.display(),
                installed_hook.display(),
                flat_hook.display()
            ),
        );
        failures += 1;
    }

    // Check 3: config dir writable (or creatable).
    let cfg = config_dir();
    let cfg_ok = cfg.exists() || std::fs::create_dir_all(&cfg).is_ok();
    if cfg_ok {
        if !emit(
            Severity::Ok,
            format!("config dir writable: {}", cfg.display()),
        ) {
            failures += 1;
        }
    } else {
        emit(
            Severity::Fail,
            format!("config dir not writable: {}", cfg.display()),
        );
        failures += 1;
    }

    // Check 4: hooks dir present/creatable + cached session count.
    let hd = hooks_dir();
    let hd_ok = hd.exists() || std::fs::create_dir_all(&hd).is_ok();
    if hd_ok {
        let count = std::fs::read_dir(&hd).map(|d| d.count()).unwrap_or(0);
        if !emit(
            Severity::Ok,
            format!("hooks dir present: {} ({count} cached sessions)", hd.display()),
        ) {
            failures += 1;
        }
    } else {
        emit(
            Severity::Fail,
            format!("hooks dir not creatable: {}", hd.display()),
        );
        failures += 1;
    }

    // Check 5: presence registry constructs cleanly. This is a
    // tautology at the moment — `PresenceRegistry::new()` cannot
    // fail. Kept as a sentinel so future refactors of
    // `server::presence` that introduce a fallible `try_new` get
    // surfaced here for free. (`emit` is unconditional; the
    // previous `if !emit(...)` pattern was dead code since the
    // Ok arm is the only one reachable.)
    let _reg = PresenceRegistry::new();
    emit(Severity::Ok, "presence registry constructs cleanly");

    // Check 6: server reachability — soft check. Only runs when an
    // env var names the server; otherwise silent. We strip a trailing
    // `/mcp` so the same `LAIN_URL` that hooks use works here without
    // requiring a separate "diagnostic" URL.
    if let Ok(url) = std::env::var("LAIN_URL").or_else(|_| std::env::var("LAIN_SERVER_URL")) {
        let health_url = format!("{}/health", url.trim_end_matches("/mcp").trim_end_matches('/'));
        match reqwest::blocking::get(&health_url) {
            Ok(r) if r.status().is_success() => {
                emit(Severity::Ok, format!("server reachable at {url}"));
            }
            Ok(r) => {
                emit(Severity::Fail, format!("server at {url} returned {}", r.status()));
                failures += 1;
            }
            Err(e) => {
                // Soft fail — `lain doctor` is also useful when no
                // server is running locally.
                emit(Severity::Warn, format!("server at {url} unreachable: {e}"));
            }
        }
    }

    println!();
    if failures == 0 {
        println!("all checks passed");
        Ok(0)
    } else {
        println!("{failures} check(s) failed");
        Ok(1)
    }
}