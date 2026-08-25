# Helper Deduplication Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate five categories of helper duplication (P0-2, P0-3, P0-5, P1-6, P1-7) by introducing canonical helpers in `cli::workspace`, `server::time`, `cli::io`, and `cli::mcp_client`, then migrating every call site. Net: ~150 LoC removed and a one-line bug killed (P0-2).

**Architecture:** Each finding follows the same shape — extract a canonical helper with a single signature, delete each local re-implementation, and adjust the caller to the canonical signature. No behavior change at any call site; this is a pure refactor. Where the existing copies disagree on return type (P0-5: `i64` / `u64` / `f64`) the canonical module exposes all three wrappers; where they disagree on signature (P0-3: `fn()` vs `fn(&Path)` vs `fn() -> Result<…>`) the canonical signature is the most general form (`fn(start: Option<&Path>) -> Result<Option<PathBuf>>`) and callers adjust.

**Tech Stack:** Rust 1.75+, `std::time::{SystemTime, UNIX_EPOCH}`, `std::path::{Path, PathBuf}`, `tempfile` (test dep, already in repo), `reqwest::blocking` (already in repo). No new dependencies.

**Source spec:** `docs/superpowers/reviews/2026-08-25-solid-dry-simplification.md` § P0-2, P0-3, P0-5, P1-6, P1-7.

---

## Global Constraints

- **No behavior change at any call site.** This is a pure refactor; if any test fails for a reason other than "the old symbol no longer exists," stop and investigate.
- **P0-2 (`resolve_repos_config`) is a latent bug.** The two definitions are byte-identical today; whichever the last editor touches wins silently. The fix is a delete, not a sync. See Task 1.
- **P0-3 canonical signature is `pub fn find_git_workspace_root(start: Option<&Path>) -> Result<Option<PathBuf>>`.** Callers that hard-coded `current_dir()` pass `None`; the `hooks.rs` variant (which took `&Path`) now passes `Some(path)`. The `init.rs` variant used anyhow `Result` — preserved by re-using `anyhow::Result`.
- **P0-5 keeps all three return types** (`i64` / `u64` / `f64`). The `f64` variant preserves `subsec_millis / 1_000.0` because `AuditEvent::ts_unix: f64` is the wire format; collapsing to integer seconds would be a behavior change and is out of scope.
- **P1-6 sync + async are separate helpers in `src/cli/io.rs`.** `write_file_atomic` (sync) and `tokio_write_file_atomic` (async) share the temp-name convention but use different IO traits. Do not collapse them behind an enum — the call sites are clearer with two named helpers.
- **P1-7 (`post_tool_call`) is HTTP-only.** `cli::oneshot.rs:108-129` speaks JSON-RPC over a child process's stdin/stdout — different transport (stdio pipes), out of scope. See Task 8.
- **Match repo test style.** Unit tests in `#[cfg(test)] mod tests` at the bottom of the file; integration in `tests/`. `cli/mcp.rs:100-152` already has a `tempfile::tempdir`-based test for this domain — mirror that style.
- **Frequent commits.** One commit per file touched; messages follow the existing imperative-mood, period-free style.
- **No `git push` and no PR creation** unless the user explicitly asks.

---

## File Structure

| Path | Change | Responsibility |
|---|---|---|
| `src/cli/mod.rs` | **Modify** | Delete duplicate `resolve_repos_config` (T1); add `pub mod workspace;` (T2), `pub mod io;` (T6), `pub mod mcp_client;` (T8) |
| `src/cli/workspace.rs` | **Create** (~40 LoC) | `find_git_workspace_root(start: Option<&Path>) -> Result<Option<PathBuf>>` |
| `src/cli/io.rs` | **Create** (~60 LoC) | `write_file_atomic` (sync) + `tokio_write_file_atomic` (async) |
| `src/cli/mcp_client.rs` | **Create** (~70 LoC) | `post_tool_call(url, name, args)` + `mcp_endpoint` + `mcp_http_client` |
| `src/cli/mcp.rs`, `init.rs`, `query.rs`, `oneshot.rs`, `hooks.rs` | **Modify** | Delete local `find_git_workspace_root` / `find_git_workspace` / `walk_up_for_git` / `find_workspace_root`; call `cli::workspace::find_git_workspace_root` |
| `src/cli/repos.rs`, `state.rs`, `server/presence.rs` | **Modify** | Delete local atomic-write boilerplate; call `cli::io::write_file_atomic` |
| `src/server/graph.rs` | **Modify** | Replace `save_to_disk` (lines 1290-1296) with `tokio_write_file_atomic`; `save_to_disk_sync` (lines 1309-1314) with `write_file_atomic` |
| `src/server/mod.rs` | **Modify** | Add `pub mod time;` |
| `src/server/time.rs` | **Create** (~30 LoC) | `unix_secs` (`i64`), `unix_secs_u64`, `unix_secs_f64` + `now_unix*` convenience wrappers |
| `src/server/federation/loader.rs`, `src/server/mcp/presence_tools.rs`, `src/server/mcp/federation_tools/server_status.rs`, `src/config/recent_projects.rs`, `src/server/presence.rs` | **Modify** | Delete local time helpers (5 sites); call `server::time::*` |
| `src/cli/hooks.rs`, `src/cli/doctor.rs` | **Modify** | Delete local JSON-RPC-over-HTTP code; call `cli::mcp_client::post_tool_call` |
| `docs/superpowers/reviews/2026-08-25-solid-dry-simplification.md` | **Modify** | Annotate P0-2, P0-3, P0-5, P1-6, P1-7 as resolved (T9) |

---

## Task 1: Delete duplicate `resolve_repos_config` (P0-2)

**Files:**
- Modify: `src/cli/mod.rs` (remove lines 18-66 — the duplicate definition and its `user_config_dir` helper)
- Modify: `src/cli/mod.rs` (replace with a one-line re-export)

**Why:** `src/lib.rs:91-123` and `src/cli/mod.rs:35-66` define `pub fn resolve_repos_config(path: &Path) -> PathBuf` with byte-identical bodies. Whichever gets edited last wins. `main.rs:50, 65, 67` calls `lain::cli::resolve_repos_config(...)`, so the CLI surface is the one callers depend on; the lib surface is presumably for external consumers. Keep `crate::resolve_repos_config` (canonical) and have the CLI module re-export it.

**Interfaces:**
- Produces (in `src/cli/mod.rs`):
  ```rust
  pub use crate::resolve_repos_config;
  ```
- Consumes (unchanged): `main.rs:50, 65, 67` (`lain::cli::resolve_repos_config(&config)`)

### Step 1: Write the failing test (asserts the re-export resolves)

In `src/cli/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::resolve_repos_config;
    use std::path::Path;

    #[test]
    fn reexport_resolves_to_canonical_implementation() {
        // Locks the property the duplicate violated. With both copies
        // present this passes (they're byte-identical); after Step 3
        // the CLI re-export and the crate-root canonical must remain
        // the same function pointer, and any future re-introduction
        // of the duplicate breaks the build.
        assert!(std::ptr::eq(
            resolve_repos_config as fn(&Path) -> std::path::PathBuf,
            crate::resolve_repos_config as fn(&Path) -> std::path::PathBuf,
        ));
    }
}
```

### Step 2: Run tests, verify they fail

```bash
cd /home/sebastian/lain
cargo test --lib cli::tests::reexport_resolves -- --nocapture
```

Expected: passes today (the duplicate is byte-identical). The test exists to lock the invariant — re-run after Step 3 to confirm it still passes against the `pub use`.

### Step 3: Delete the duplicate and add the re-export

In `src/cli/mod.rs`:

1. Delete the entire `pub fn resolve_repos_config(...)` block at lines 18-66 (and the `user_config_dir` helper at lines 59-66 — that helper is also private to this module and is only used by the deleted function).

2. Add the re-export immediately after `pub use server::run_server;` (line 16):

```rust
pub use crate::resolve_repos_config;
```

### Step 4: Run tests, verify they pass; run full CLI surface

```bash
cd /home/sebastian/lain
cargo test --lib cli::tests::reexport_resolves -- --nocapture
cargo build
cargo test --lib cli::
cargo test --test cli_surface
```

Expected: 1 passed; `cargo build` clean; all CLI tests pass.

### Step 5: Commit

```bash
git add src/cli/mod.rs
git commit -m "Delete duplicate cli::resolve_repos_config in favor of crate:: re-export"
```

---

## Task 2: Create `cli::workspace::find_git_workspace_root` (P0-3, helper)

**Files:**
- Create: `src/cli/workspace.rs` (~40 LoC)
- Modify: `src/cli/mod.rs` (add `pub mod workspace;`)

**Why:** 5 CLI files walk up from a directory looking for `.git`. Each is a near-cousin — the only real differences are return type (`Option<PathBuf>` vs `Result<Option<PathBuf>>`), whether they canonicalize the start dir, and whether they take an explicit start path. Centralize as the canonical signature.

**Interfaces:**
- Produces (in `src/cli/workspace.rs`):
  ```rust
  use std::path::{Path, PathBuf};
  use anyhow::{Context, Result};

  /// Walk up from `start` (defaulting to the current working directory)
  /// until a directory containing `.git` is found, and return that
  /// ancestor. Returns `Ok(None)` when no `.git` is found within 16
  /// levels or the start path cannot be resolved.
  ///
  /// `start = None` uses `std::env::current_dir()` and canonicalizes
  /// it; `start = Some(p)` uses `p.canonicalize()` (falling back to
  /// `p.to_path_buf()` on canonicalize failure, mirroring the prior
  /// `hooks.rs::find_workspace_root` behavior so filesystem-only
  /// paths under a not-yet-created worktree don't error out).
  pub fn find_git_workspace_root(start: Option<&Path>) -> Result<Option<PathBuf>> {
      let mut current = match start {
          Some(p) => p
              .canonicalize()
              .unwrap_or_else(|_| p.to_path_buf()),
          None => std::env::current_dir()
              .context("get current dir")?
              .canonicalize()
              .context("canonicalize cwd")?,
      };
      for _ in 0..16 {
          if current.join(".git").exists() {
              return Ok(Some(current));
          }
          match current.parent() {
              Some(p) => current = p.to_path_buf(),
              None => return Ok(None),
          }
      }
      Ok(None)
  }
  ```

### Step 1: Write the failing tests

Append a `#[cfg(test)] mod tests` block to `src/cli/workspace.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn returns_some_when_dot_git_is_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join(".git")).unwrap();
        let sub = root.join("src").join("nested");
        fs::create_dir_all(&sub).unwrap();
        let found = find_git_workspace_root(Some(&sub)).unwrap();
        // canonicalize normalizes /tmp -> /private/tmp on macOS; just
        // assert we walked up to *some* directory containing `.git`.
        assert!(found.unwrap().join(".git").exists());
    }

    #[test]
    fn returns_none_when_no_dot_git_within_16() {
        let tmp = tempfile::tempdir().unwrap();
        // No .git anywhere up the tempdir chain (tempdir parents don't
        // contain .git in practice; assert that explicitly.)
        let found = find_git_workspace_root(Some(tmp.path())).unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn none_start_uses_current_dir() {
        // Cannot easily test the cwd path without mutating env, but we
        // can assert the call signature compiles and returns Ok.
        let result = find_git_workspace_root(None);
        assert!(result.is_ok());
    }
}
```

### Step 2: Run tests, verify they fail

```bash
cd /home/sebastian/lain
cargo test --lib cli::workspace::tests -- --nocapture
```

Expected: `error[E0433]: failed to resolve: use of undeclared module 'workspace'` (because `pub mod workspace;` hasn't been added yet).

### Step 3: Implement the helper

Create `src/cli/workspace.rs` with the body from the **Interfaces** section above. Then in `src/cli/mod.rs` add `pub mod workspace;` alongside the other `pub mod` declarations (line 1-12).

### Step 4: Run tests, verify they pass

```bash
cd /home/sebastian/lain
cargo test --lib cli::workspace::tests -- --nocapture
```

Expected: 3 passed.

### Step 5: Commit

```bash
git add src/cli/workspace.rs src/cli/mod.rs
git commit -m "Add cli::workspace::find_git_workspace_root canonical helper"
```

---

## Task 3: Migrate the 5 callers of `find_git_workspace_root` (P0-3, callers)

**Files (one commit per file):**
- Modify: `src/cli/mcp.rs` (lines 83-98 + tests at 100-152 that referenced the local symbol)
- Modify: `src/cli/init.rs` (lines 71-84)
- Modify: `src/cli/query.rs` (lines 219-228)
- Modify: `src/cli/oneshot.rs` (lines 220-233)
- Modify: `src/cli/hooks.rs` (lines 559-580)

**Interfaces:**
- Consumes: `crate::cli::workspace::find_git_workspace_root(Option<&Path>) -> Result<Option<PathBuf>>` (from Task 2)
- Produces: each file's local helper removed; every call site uses the canonical helper

### Step 1: Migrate `cli/mcp.rs`

Delete the local `fn find_git_workspace_root()` at `src/cli/mcp.rs:83-98`. Update the single call site and the test (`src/cli/mcp.rs:100-152`, which calls the local symbol) to `crate::cli::workspace::find_git_workspace_root(None)` and `Some(&sub)` respectively. The test's assertion (`found.unwrap().join(".git").exists()`) stays valid.

```bash
cargo test --lib cli::mcp::tests -- --nocapture
git add src/cli/mcp.rs
git commit -m "Route cli::mcp through cli::workspace::find_git_workspace_root"
```

### Step 2: Migrate `cli/init.rs`

Local `find_git_workspace` (lines 71-84) returns anyhow `Result<Option<PathBuf>>` — identical to the new helper. Replace its definition with `pub(crate) use crate::cli::workspace::find_git_workspace_root;`. Call site needs no update.

```bash
cargo test --lib cli::init::tests -- --nocapture
git add src/cli/init.rs
git commit -m "Route cli::init through cli::workspace::find_git_workspace_root"
```

### Step 3: Migrate `cli/query.rs`

Local `walk_up_for_git` (lines 219-228) returns `Option<PathBuf>` (no `Result`). Replace with a call to the helper; the single call site is at `fn main` level — wrap with `.ok().flatten()` (or propagate with `?` if deeper). No other code path consumes the return value.

```bash
cargo test --lib cli::query::tests -- --nocapture
git add src/cli/query.rs
git commit -m "Route cli::query through cli::workspace::find_git_workspace_root"
```

### Step 4: Migrate `cli/oneshot.rs`

Local `find_git_workspace` (lines 220-233) returns anyhow `Result<Option<PathBuf>>`. Add `use crate::cli::workspace::find_git_workspace_root;` and update the call site to pass `None` (it used `current_dir`).

```bash
cargo test --lib cli::oneshot::tests -- --nocapture
git add src/cli/oneshot.rs
git commit -m "Route cli::oneshot through cli::workspace::find_git_workspace_root"
```

### Step 5: Migrate `cli/hooks.rs`

Local `find_workspace_root` (lines 559-580) takes `&Path` and always returns a `PathBuf` (falls back to `path.parent()` when no `.git` is found). Call `cli::workspace::find_git_workspace_root(Some(path))` and wrap with `.unwrap_or_else(|| path.parent().map(...).unwrap_or_else(|| path.to_path_buf()))` to preserve the fallback semantics.

```bash
cargo test --lib cli::hooks::tests -- --nocapture
git add src/cli/hooks.rs
git commit -m "Route cli::hooks through cli::workspace::find_git_workspace_root"
```

### Step 6: Verify no remaining local definitions

```bash
cd /home/sebastian/lain
grep -rn "fn find_git_workspace\|fn walk_up_for_git\|fn find_workspace_root" src/
```

Expected: zero hits in production files (the canonical `pub fn find_git_workspace_root` in `src/cli/workspace.rs` is the only definition).

### Step 7: Full test sweep

```bash
cd /home/sebastian/lain
cargo test --lib
cargo test --test cli_surface
cargo test --test doctor_smoke
cargo test --test e2e_behavior 2>/dev/null | tail -20
```

Expected: all pass; no behavior change.

---

## Task 4: Create `server::time` helpers (P0-5, helpers)

**Files:**
- Create: `src/server/time.rs` (~30 LoC)
- Modify: `src/server/mod.rs` (add `pub mod time;`)

**Why:** Five files each define their own `system_time_to_unix_secs` / `now_unix` / `system_time_now_unix` with three different return types (`i64`, `u64`, `f64`). The return types are load-bearing — the `f64` variant carries sub-second precision because `AuditEvent::ts_unix` is `f64`. Centralize all three.

**Interfaces:**
- Produces (in `src/server/time.rs`):
  ```rust
  use std::time::{Duration, SystemTime, UNIX_EPOCH};

  /// Seconds since the epoch, as `i64`. Pre-epoch collapses to 0
  /// rather than underflowing (saturating `duration_since`).
  pub fn unix_secs(t: SystemTime) -> i64 {
      t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
  }

  /// Seconds since the epoch, as `u64`. Pre-epoch collapses to 0.
  pub fn unix_secs_u64(t: SystemTime) -> u64 {
      t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
  }

  /// Seconds since the epoch with millisecond precision, as `f64`.
  /// Matches `AuditEvent::ts_unix: f64` so persistence and reads stay
  /// in the same unit (loaders that parsed sub-second audit events
  /// would break if this collapsed to integer seconds).
  pub fn unix_secs_f64(t: SystemTime) -> f64 {
      let dur = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
      dur.as_secs() as f64 + dur.subsec_millis() as f64 / 1_000.0
  }

  /// Convenience wrapper for the common "now" case.
  pub fn now_unix() -> i64 { unix_secs(SystemTime::now()) }
  pub fn now_unix_u64() -> u64 { unix_secs_u64(SystemTime::now()) }
  pub fn now_unix_f64() -> f64 { unix_secs_f64(SystemTime::now()) }
  ```

### Step 1: Write the failing tests

In `src/server/time.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn unix_secs_returns_seconds_since_epoch() {
        let t = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(unix_secs(t), 1_700_000_000);
    }

    #[test]
    fn unix_secs_collapses_pre_epoch_to_zero() {
        let t = UNIX_EPOCH - Duration::from_secs(10);
        assert_eq!(unix_secs(t), 0);
    }

    #[test]
    fn unix_secs_u64_returns_seconds() {
        let t = UNIX_EPOCH + Duration::from_secs(42);
        assert_eq!(unix_secs_u64(t), 42);
    }

    #[test]
    fn unix_secs_f64_includes_millis() {
        let t = UNIX_EPOCH + Duration::from_secs(100) + Duration::from_millis(250);
        // 100.250
        let got = unix_secs_f64(t);
        assert!((got - 100.25).abs() < 1e-6, "got: {got}");
    }

    #[test]
    fn now_unix_is_close_to_wall_clock() {
        let before = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let n = now_unix();
        let after = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        assert!(n >= before && n <= after, "n={n}, before={before}, after={after}");
    }
}
```

### Step 2: Run tests, verify they fail

```bash
cd /home/sebastian/lain
cargo test --lib server::time::tests -- --nocapture
```

Expected: `error[E0433]: failed to resolve: use of undeclared module 'time'`.

### Step 3: Implement the helpers

Create `src/server/time.rs` with the body from the **Interfaces** section above. In `src/server/mod.rs`, add `pub mod time;` alongside the other `pub mod` declarations.

### Step 4: Run tests, verify they pass

```bash
cargo test --lib server::time::tests -- --nocapture
```

Expected: 5 passed.

### Step 5: Commit

```bash
git add src/server/time.rs src/server/mod.rs
git commit -m "Add server::time helpers (unix_secs, unix_secs_u64, unix_secs_f64)"
```

---

## Task 5: Migrate the 5 callers of `system_time_to_unix_secs` (P0-5, callers)

**Files (one commit per file):**
- Modify: `src/server/federation/loader.rs` (lines 186-191, returns `i64`)
- Modify: `src/server/mcp/presence_tools.rs` (lines 687-691, returns `u64` + delta)
- Modify: `src/server/mcp/federation_tools/server_status.rs` (lines 14-16, returns `i64`)
- Modify: `src/config/recent_projects.rs` (lines 36-44, `now_unix -> i64`)
- Modify: `src/server/presence.rs` (lines 2055-2065, returns `f64`)

**Interfaces:**
- Consumes: `crate::server::time::{unix_secs, unix_secs_u64, unix_secs_f64, now_unix, now_unix_u64, now_unix_f64}` (from Task 4)
- Produces: each file's local helper deleted; every call site uses the canonical helper

### Step 1: Migrate `federation/loader.rs`

Local `fn system_time_to_unix_secs(t) -> i64` (lines 186-191) is byte-identical to `crate::server::time::unix_secs`. Delete it; replace the call site at line 186 with `crate::server::time::unix_secs(t)`. Add `use crate::server::time;` at the top.

```bash
cargo test --lib server::federation::loader::tests -- --nocapture
git add src/server/federation/loader.rs
git commit -m "Route federation::loader through server::time::unix_secs"
```

### Step 2: Migrate `mcp/presence_tools.rs`

Delete local `system_time_to_unix_secs` (lines 687-688) and `system_time_to_unix_secs_delta` (line 691). Replace call sites:
- Line 683 (`system_time_to_unix_secs(c.claimed_at)`) → `crate::server::time::unix_secs_u64(c.claimed_at)`
- `system_time_to_unix_secs_delta(d)` → `d.as_secs()` (inline one-liner; no helper needed)

```bash
cargo test --lib server::mcp::presence_tools::tests -- --nocapture
git add src/server/mcp/presence_tools.rs
git commit -m "Route mcp::presence_tools through server::time::unix_secs_u64"
```

### Step 3: Migrate `mcp/federation_tools/server_status.rs`

Delete local `fn system_time_to_unix` (lines 14-16); replace call sites with `crate::server::time::unix_secs(t)`.

```bash
cargo test --lib server::mcp::federation_tools::server_status::tests -- --nocapture
git add src/server/mcp/federation_tools/server_status.rs
git commit -m "Route federation_tools::server_status through server::time::unix_secs"
```

### Step 4: Migrate `config/recent_projects.rs`

Delete local `fn now_unix()` (lines 36-44); replace `now_unix()` calls with `crate::server::time::now_unix()`.

```bash
cargo test --lib config::recent_projects::tests -- --nocapture
git add src/config/recent_projects.rs
git commit -m "Route config::recent_projects through server::time::now_unix"
```

### Step 5: Migrate `server/presence.rs`

Delete local `fn system_time_now_unix() -> f64` (lines 2055-2065); replace the two call sites (lines 1639 and 2004) with `crate::server::time::now_unix_f64()`.

```bash
cargo test --lib server::presence::tests -- --nocapture
cargo test --test presence_e2e
git add src/server/presence.rs
git commit -m "Route server::presence through server::time::now_unix_f64"
```

### Step 6: Verify no remaining local definitions

```bash
cd /home/sebastian/lain
grep -rn "fn system_time_to_unix_secs\|fn system_time_to_unix_secs_delta\|fn system_time_to_unix\b\|fn system_time_now_unix\|fn now_unix\b" src/
```

Expected: only the canonical definitions in `src/server/time.rs`; zero local copies.

### Step 7: Full test sweep

```bash
cd /home/sebastian/lain
cargo test --lib
cargo test --test federation_integration
cargo test --test audit_integration
```

Expected: all pass.

---

## Task 6: Create `cli::io::write_file_atomic` and migrate sync callers (P1-6 sync)

**Files:**
- Create: `src/cli/io.rs` (~60 LoC, sync helper only — async is Task 7)
- Modify: `src/cli/mod.rs` (add `pub mod io;`)
- Modify: `src/cli/repos.rs` (delete `write_atomic`, lines 86-99)
- Modify: `src/state.rs` (replace inline atomic write in `ActiveWorkspace::save`, lines 64-80)
- Modify: `src/server/presence.rs` (replace inline atomic writes at lines 1575-1585 and 1650-1658)

**Why:** Three files each hand-roll "write to `path.tmp`, rename over `path`" with subtly different conventions (tmp-name shape, mkdir-before-write semantics, error mapping). Centralize.

**Interfaces:**
- Produces (in `src/cli/io.rs`):
  ```rust
  use std::fs;
  use std::io;
  use std::path::Path;

  /// Write `bytes` to `path` atomically by writing to a sibling temp
  /// file first and renaming it over `path`. Creates the parent
  /// directory if it doesn't exist. The temp file uses
  /// `path.with_extension("tmp")` for compatibility with every
  /// existing caller (the prior `repos.rs::write_atomic` used
  /// `.{name}.tmp`; the canonical helper uses `with_extension` to
  /// match `state.rs` and `presence.rs`, and the rename semantics
  /// are equivalent).
  pub fn write_file_atomic(path: &Path, bytes: impl AsRef<[u8]>) -> io::Result<()> {
      if let Some(parent) = path.parent() {
          if !parent.as_os_str().is_empty() {
              fs::create_dir_all(parent)?;
          }
      }
      let tmp = path.with_extension("tmp");
      fs::write(&tmp, bytes)?;
      fs::rename(&tmp, path)
  }
  ```

### Step 1: Write the failing tests

In `src/cli/io.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_file_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("data.json");
        write_file_atomic(&path, b"hello").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");
    }

    #[test]
    fn creates_parent_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("deeper").join("f.txt");
        write_file_atomic(&path, b"x").unwrap();
        assert!(path.exists());
    }

    #[test]
    fn overwrites_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.txt");
        write_file_atomic(&path, b"first").unwrap();
        write_file_atomic(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
    }

    #[test]
    fn no_tmp_file_left_behind_on_success() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.txt");
        write_file_atomic(&path, b"ok").unwrap();
        // The .tmp sibling should be gone (rename moved it).
        assert!(!path.with_extension("tmp").exists());
    }
}
```

### Step 2: Run tests, verify they fail

```bash
cd /home/sebastian/lain
cargo test --lib cli::io::tests -- --nocapture
```

Expected: `error[E0433]: failed to resolve: use of undeclared module 'io'`.

### Step 3: Implement the helper

Create `src/cli/io.rs` with the body from the **Interfaces** section above. Add `pub mod io;` to `src/cli/mod.rs`.

### Step 4: Run tests, verify they pass

```bash
cargo test --lib cli::io::tests -- --nocapture
```

Expected: 4 passed.

### Step 5: Migrate `cli/repos.rs`

Delete `fn write_atomic<T: serde::Serialize>(...)` (lines 86-99). Replace its caller:

```rust
let yaml = serde_yaml::to_string(value).context("serialize yaml")?;
crate::cli::io::write_file_atomic(path, yaml.as_bytes())
    .with_context(|| format!("write {}", path.display()))?;
```

The tmp-file name changes from `.{name}.tmp` to `{name}.tmp`; the rename target is identical and no reader sees the temp file. Acceptable per Global Constraints.

```bash
cargo test --lib cli::repos::tests -- --nocapture
git add src/cli/repos.rs src/cli/io.rs src/cli/mod.rs
git commit -m "Add cli::io::write_file_atomic; migrate cli::repos"
```

### Step 6: Migrate `state.rs`

Replace the inline atomic write in `ActiveWorkspace::save` (lines 64-80) with:

```rust
let text = match &self.config_path {
    Some(p) => format!("{}\n{}\n", p.display(), self.name),
    None => format!("{}\n", self.name),
};
crate::cli::io::write_file_atomic(&active_workspace_file(), text.as_bytes())
    .map_err(|e| LainError::Io(e.to_string()))?;
```

The prior code used `path.with_extension("tmp")` — same as the helper, so the temp-file shape is unchanged. Remove the now-unused `config_dir()` call inside `save` if it's only used for the manual `create_dir_all`; the helper does its own mkdir. Verify `config_dir` is still used elsewhere in `state.rs` before removing.

```bash
cargo test --lib state::tests -- --nocapture
```

```bash
git add src/state.rs
git commit -m "Migrate state::ActiveWorkspace::save to cli::io::write_file_atomic"
```

### Step 7: Migrate `server/presence.rs`

Two sites:

**Site A — `save_pair` (lines 1575-1585):** the helper owns it; drop the explicit `create_dir_all` and replace the `write + rename` block with:

```rust
crate::cli::io::write_file_atomic(path, json.as_bytes())
    .map_err(|e| format!("write {}: {e}", path.display()))?;
```

**Site B — `load_pair` reset branch (lines 1650-1658):** same replacement.

```bash
cargo test --lib server::presence::tests -- --nocapture
cargo test --test presence_e2e
cargo test --test presence_lock
git add src/server/presence.rs
git commit -m "Migrate server::presence atomic writes to cli::io::write_file_atomic"
```

### Step 8: Verify

```bash
cd /home/sebastian/lain
grep -rn "with_extension.*tmp\|fs::write.*\.tmp\|fs::rename" src/cli/repos.rs src/state.rs src/server/presence.rs
```

Expected: zero matches inside the three migrated files (the helper owns the rename).

---

## Task 7: Migrate `graph.rs` to `tokio_write_file_atomic` (P1-6 async)

**Files:**
- Modify: `src/cli/io.rs` (add `tokio_write_file_atomic`)
- Modify: `src/server/graph.rs` (lines 1290-1296 and 1309-1314)

**Why:** `graph.rs::save_to_disk` (async, `tokio::fs`) and `save_to_disk_sync` (sync) implement the same "write-tmp-then-rename" pattern. Add a tokio-flavored helper alongside the sync one; graph is the only async caller today.

**Interfaces:**
- Produces (added to `src/cli/io.rs`):
  ```rust
  /// Async counterpart to `write_file_atomic` for callers running
  /// inside a Tokio runtime. Currently consumed by
  /// `server::graph::save_to_disk`.
  pub async fn tokio_write_file_atomic(
      path: &Path,
      bytes: impl AsRef<[u8]>,
  ) -> io::Result<()> {
      if let Some(parent) = path.parent() {
          if !parent.as_os_str().is_empty() {
              tokio::fs::create_dir_all(parent).await?;
          }
      }
      let tmp = path.with_extension("tmp");
      tokio::fs::write(&tmp, bytes).await?;
      tokio::fs::rename(&tmp, path).await
  }
  ```

### Step 1: Write the failing test

In `src/cli/io.rs`'s existing test module, add:

```rust
#[tokio::test]
async fn tokio_write_file_atomic_writes_and_renames() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("graph.bin");
    tokio_write_file_atomic(&path, b"\x01\x02\x03").await.unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"\x01\x02\x03");
    assert!(!path.with_extension("tmp").exists());
}

#[tokio::test]
async fn tokio_write_file_atomic_creates_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a/b/c/state.bin");
    tokio_write_file_atomic(&path, b"x").await.unwrap();
    assert!(path.exists());
}
```

### Step 2: Run tests, verify they fail

```bash
cd /home/sebastian/lain
cargo test --lib cli::io::tests::tokio -- --nocapture
```

Expected: `error[E0425]: cannot find function tokio_write_file_atomic`.

### Step 3: Implement the helper

Add the function from the **Interfaces** block to `src/cli/io.rs`. Add `use tokio;` at the top of the file (or qualify the call inline — match the style of the rest of the file).

### Step 4: Run tests, verify they pass

```bash
cargo test --lib cli::io::tests -- --nocapture
```

Expected: 6 passed (4 sync + 2 async).

### Step 5: Migrate `graph.rs::save_to_disk` (async, lines 1290-1296)

Replace:

```rust
if let Some(parent) = persistence_path.parent() {
    tokio::fs::create_dir_all(parent).await.map_err(|e| LainError::Database(e.to_string()))?;
}
tokio::fs::write(&tmp_path, data).await.map_err(|e| LainError::Database(e.to_string()))?;
tokio::fs::rename(&tmp_path, &persistence_path).await.map_err(|e| LainError::Database(e.to_string()))?;
```

with:

```rust
crate::cli::io::tokio_write_file_atomic(&persistence_path, &data)
    .await
    .map_err(|e| LainError::Database(e.to_string()))?;
```

### Step 6: Migrate `graph.rs::save_to_disk_sync` (sync, lines 1309-1314)

Replace:

```rust
if let Some(parent) = self.persistence_path.parent() {
    std::fs::create_dir_all(parent).map_err(|e| LainError::Database(e.to_string()))?;
}
let tmp_path = self.persistence_path.with_extension("tmp");
std::fs::write(&tmp_path, data).map_err(|e| LainError::Database(e.to_string()))?;
std::fs::rename(&tmp_path, &self.persistence_path).map_err(|e| LainError::Database(e.to_string()))?;
```

with:

```rust
crate::cli::io::write_file_atomic(&self.persistence_path, &data)
    .map_err(|e| LainError::Database(e.to_string()))?;
```

### Step 7: Run tests, verify pass

```bash
cd /home/sebastian/lain
cargo test --lib server::graph::tests -- --nocapture
cargo test --test e2e_behavior 2>/dev/null | tail -20
```

Expected: all pass.

### Step 8: Commit

```bash
git add src/cli/io.rs src/server/graph.rs
git commit -m "Add tokio_write_file_atomic; migrate server::graph save_to_disk"
```

---

## Task 8: Create `cli::mcp_client::post_tool_call` and migrate HTTP callers (P1-7)

**Files:**
- Create: `src/cli/mcp_client.rs` (~70 LoC)
- Modify: `src/cli/mod.rs` (add `pub mod mcp_client;`)
- Modify: `src/cli/hooks.rs` (delete `McpRequest`/`McpResponse`/`post_mcp` and the typed-struct parade at lines 207-304; replace with `cli::mcp_client::post_tool_call`)
- Modify: `src/cli/doctor.rs` (replace `emit_tools_list_check` body at lines 63-114 with the helper)

**Why:** Two files each hand-roll the same JSON-RPC-over-HTTP dance: build the envelope, build the reqwest client, POST, check status, parse, check `error`, unwrap `result`. Centralize.

**`oneshot.rs` is out of scope:** it speaks JSON-RPC over a child process's stdin/stdout — different transport (stdio pipes, line-delimited, no reqwest). The new helper takes a `url`; it doesn't fit `oneshot.rs`'s shape. Left alone; address by a future "stdio MCP client" task if pressure grows.

**Interfaces:**
- Produces (in `src/cli/mcp_client.rs`):
  ```rust
  use std::time::Duration;
  use anyhow::{anyhow, Context, Result};
  use serde_json::{json, Value};

  /// Shared reqwest blocking client (2 s request, 500 ms connect).
  /// A wedged server can't hang the caller for the OS's full TCP
  /// connect timeout (~75 s on Linux).
  fn mcp_http_client() -> reqwest::blocking::Client {
      reqwest::blocking::Client::builder()
          .timeout(Duration::from_secs(2))
          .connect_timeout(Duration::from_millis(500))
          .build()
          .unwrap_or_else(|_| reqwest::blocking::Client::new())
  }

  /// Normalize `url` to the canonical MCP endpoint:
  /// bare URL → append `/mcp`; full URL unchanged; trailing `/` stripped.
  pub fn mcp_endpoint(url: &str) -> String {
      let trimmed = url.trim_end_matches('/');
      if trimmed.ends_with("/mcp") {
          trimmed.to_string()
      } else {
          format!("{trimmed}/mcp")
      }
  }

  /// Issue a `tools/call` JSON-RPC request to `url` and return the
  /// response's `result` field as `serde_json::Value`. The `id: 1`
  /// is fixed (the lain server is single-threaded per request).
  pub fn post_tool_call(url: &str, name: &str, args: Value) -> Result<Value> {
      let endpoint = mcp_endpoint(url);
      let body = json!({
          "jsonrpc": "2.0",
          "id": 1,
          "method": "tools/call",
          "params": {"name": name, "arguments": args},
      });
      let client = mcp_http_client();
      let resp = client.post(&endpoint).json(&body).send()
          .context("HTTP send")?;
      if !resp.status().is_success() {
          return Err(anyhow!("HTTP {} from lain server", resp.status()));
      }
      let value: Value = resp.json().context("parse JSON-RPC response")?;
      if let Some(err) = value.get("error") {
          return Err(anyhow!("MCP error: {err}"));
      }
      value.get("result").cloned()
          .ok_or_else(|| anyhow!("no result in MCP response"))
  }
  ```

### Step 1: Write the failing tests

In `src/cli/mcp_client.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_endpoint_appends_mcp_path() {
        assert_eq!(mcp_endpoint("http://localhost:9999"), "http://localhost:9999/mcp");
        assert_eq!(mcp_endpoint("http://localhost:9999/"), "http://localhost:9999/mcp");
    }

    #[test]
    fn mcp_endpoint_strips_trailing_slash_on_full_url() {
        assert_eq!(mcp_endpoint("http://localhost:9999/mcp"), "http://localhost:9999/mcp");
        assert_eq!(mcp_endpoint("http://localhost:9999/mcp/"), "http://localhost:9999/mcp");
    }
}
```

(Network behavior of `post_tool_call` is exercised by the existing `doctor_smoke` and `hooks` integration tests in `tests/`. Do not add network-mock tests here — match the repo's existing test style.)

### Step 2: Run tests, verify they fail

```bash
cd /home/sebastian/lain
cargo test --lib cli::mcp_client::tests -- --nocapture
```

Expected: `error[E0433]: failed to resolve: use of undeclared module 'mcp_client'`.

### Step 3: Implement the helper

Create `src/cli/mcp_client.rs` with the body from the **Interfaces** section above. Add `pub mod mcp_client;` to `src/cli/mod.rs`.

### Step 4: Run tests, verify they pass

```bash
cargo test --lib cli::mcp_client::tests -- --nocapture
```

Expected: 2 passed.

### Step 5: Migrate `cli/hooks.rs`

Delete the typed-struct parade at lines 207-231 (`McpRequest`, `McpResponse`, `McpResult`, `McpContent`) and `post_mcp` at lines 286-304. Replace each call site of `post_mcp(...)` with `crate::cli::mcp_client::post_tool_call(url, method, params).context(...)?`. The `mcp_endpoint` helper (lines 262-269) becomes `use crate::cli::mcp_client::mcp_endpoint;`.

The prior `post_mcp` returned `Result<McpResult>` (typed content array); the new helper returns `Result<serde_json::Value>`. Adjust callers: `text_of(r: McpResult)` (lines 306-315) goes away. If a caller needs the first content text, use `value["content"][0]["text"].as_str().unwrap_or("")`.

```bash
cargo test --lib cli::hooks::tests -- --nocapture
cargo build
git add src/cli/hooks.rs src/cli/mcp_client.rs src/cli/mod.rs
git commit -m "Extract cli::mcp_client::post_tool_call; migrate cli::hooks"
```

### Step 6: Migrate `cli/doctor.rs`

Replace the body of `emit_tools_list_check` (lines 63-114). The new body is:

```rust
fn emit_tools_list_check(base: &str) -> bool {
    let url = format!("{base}/mcp");
    let value = match crate::cli::mcp_client::post_tool_call(&url, "tools/list", serde_json::json!({})) {
        Ok(v) => v,
        Err(e) => return emit(
            Severity::Fail,
            format!("MCP endpoint {url} did not answer tools/list: {e}"),
        ),
    };
    let tools = value
        .get("tools")
        .and_then(|t| t.as_array());
    match tools {
        Some(list) if !list.is_empty() => emit(
            Severity::Ok,
            format!("MCP surface live: tools/list advertises {} tools", list.len()),
        ),
        Some(_) => emit(
            Severity::Fail,
            "MCP surface empty: tools/list advertises 0 tools (agents will see no tools)",
        ),
        None => emit(
            Severity::Fail,
            "tools/list response had no result.tools array",
        ),
    }
}
```

The old `emit_tools_list_check` did the JSON-RPC dance by hand; the new body uses the helper. HTTP-failure, error-envelope, and parse-failure cases now flow through the helper's `anyhow::Error`, which `doctor.rs` formats identically.

```bash
cargo test --lib cli::doctor::tests -- --nocapture
cargo test --test doctor_smoke
```

```bash
git add src/cli/doctor.rs
git commit -m "Migrate cli::doctor::emit_tools_list_check to cli::mcp_client::post_tool_call"
```

### Step 7: Verify no remaining hand-rolled JSON-RPC-over-HTTP

```bash
cd /home/sebastian/lain
grep -rn "jsonrpc.*2\.0\|McpRequest\|McpResponse\|McpResult\|McpContent" src/cli/hooks.rs src/cli/doctor.rs
```

Expected: zero hits in the two migrated files (the canonical helpers and the test fixtures in `cli/mcp_client.rs` are the only definitions).

---

## Task 9: Final sweep — annotate the report and verify the test surface

**Files:**
- Modify: `docs/superpowers/reviews/2026-08-25-solid-dry-simplification.md`

### Step 1: Annotate the review

Append `**Resolved by:** plan 2026-08-25-helper-deduplication, Task N.` after each finding:
- § 3 P0-2 → Task 1
- § 3 P0-3 → Tasks 2-3
- § 3 P0-5 → Tasks 4-5
- § 4 P1-6 → Tasks 6-7
- § 4 P1-7 → Task 8 (HTTP only; `cli::oneshot.rs` stdio case deferred)

### Step 2: Verify LoC and tests

```bash
cd /home/sebastian/lain
wc -l src/cli/{mcp,init,query,oneshot,hooks,repos,doctor}.rs src/state.rs \
      src/server/presence.rs src/server/graph.rs \
      src/cli/{workspace,io,mcp_client}.rs src/server/time.rs
cargo test --lib && cargo test --tests
```

Expected: total LoC roughly even (5 local copies replaced by 1 helper + tests), but exactly one place to evolve each helper. All tests pass.

### Step 3: Commit

```bash
git add docs/superpowers/reviews/2026-08-25-solid-dry-simplification.md
git commit -m "docs: annotate P0-2, P0-3, P0-5, P1-6, P1-7 as resolved by helper-deduplication plan"
```

---

## Self-Review (do before handing to user)

After writing this plan, verify:

1. **Spec coverage:** P0-2 → Task 1. P0-3 → Tasks 2-3. P0-5 → Tasks 4-5. P1-6 → Tasks 6-7. P1-7 → Task 8. ✅
2. **Placeholder scan:** No "TODO" / "TBD" / "fill in" in any task body. Code blocks show actual signatures and code. The only deliberate scope-decision note is Task 8's "oneshot.rs is out of scope," which is justified inline. ✅
3. **Type consistency:** Each helper defined in its create task is consumed in the matching migrate task; `find_git_workspace_root` (Task 2 → Task 3), `unix_secs*` (Task 4 → Task 5), `write_file_atomic` (Task 6 → Task 7's `save_to_disk_sync` + Task 6's own migrations), `tokio_write_file_atomic` (Task 7 → `save_to_disk`), `post_tool_call` (Task 8 → `hooks.rs` + `doctor.rs`). ✅
4. **Bite-sized steps:** Each step is 2-5 minutes; the largest is Task 8 Step 5 (mechanical) and Task 5 Step 5 (the only one requiring reasoning about sub-second precision, documented in the body comment). ✅
5. **Repo conventions:** TDD matches the existing style (`cli/mcp.rs::find_git_workspace_root_walks_up_to_dot_git` for P0-3; `#[cfg(test)] mod tests` blocks for all new helpers). No new files in `tests/` — the existing integration tests exercise the helpers through their callers. ✅

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-08-25-helper-deduplication.md`.

**Estimated total effort:** 9 tasks, ~2-4 working days. Task 1 is five minutes; Tasks 6-8 each touch 2-4 files and are the bulk of the work.

**Risks:**

- **Task 5 (`presence.rs` migration to `f64` helper) is the most likely spot for a subtle bug.** The local `system_time_now_unix` returns `f64` with `subsec_millis / 1_000.0` precision and feeds `AuditEvent::ts_unix: f64`. Mitigation: Task 4's `unix_secs_f64` test asserts `100.250 == 100.25`; Task 5 runs `cargo test --test audit_integration`.
- **Task 6 changes the `cli::repos.rs` temp-file name from `.{name}.tmp` to `{name}.tmp`.** Rename target is identical; no reader sees the temp file. Mitigation: `git grep` for `\.repos\.yaml\.tmp` before merging.
- **Task 8's `hooks.rs` migration changes the return shape from `McpResult` (typed) to `serde_json::Value`.** Callers that did `text_of(r)` must switch to `value["content"][0]["text"].as_str()`. Mitigation: `cargo build` after the edit catches every caller.
- **Task 7's `graph.rs` migration** preserves `with_extension("tmp")` byte-for-byte (lines 1284 and 1312 already use it), so no Windows-specific rename change.

Two execution options:

**1. Subagent-Driven (recommended)** — Dispatch a fresh subagent per task; Tasks 1, 2, 4, 6, 7, 8 each create a new file with tests and run independently; Tasks 3, 5, 9 are best done by a single agent that has the full migration list in scope.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints. Best if you want to do the review yourself and keep related-file migrations atomic.

Which approach?