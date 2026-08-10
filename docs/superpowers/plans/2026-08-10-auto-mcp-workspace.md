# Auto-detect Workspace for MCP Server Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a single install of the Lain MCP server work in any git repository by detecting the workspace from the agent's current working directory at startup.

**Architecture:** Treat `--workspace auto` as a sentinel. When Lain sees it, it uses `git2::Repository::discover(".")` to find the repository root and serve that repo. `lain init` and `lain agents install` write the sentinel into every agent's MCP config so one config covers every repo.

**Tech Stack:** Rust, `git2` (already a dependency), `clap`, `anyhow`, `tracing`.

## Global Constraints

- `--workspace auto` is the only new sentinel value. Existing absolute paths continue to work unchanged.
- The sentinel must be resolved **before** any subcommand dispatch in `main`, so `init`, `query`, and the default server path all benefit.
- The default clap value for `--workspace` stays `"."`. The sentinel is write‑only by the installers.
- Bare git repositories are rejected with a clear error message.
- All changes stay local to the repository; no new top-level dependencies.

---

## File Structure

| File | Change |
|------|--------|
| `src/state.rs` | Add `resolve_auto_workspace()` and unit tests. |
| `src/main.rs` | Resolve the sentinel before subcommand dispatch and log the chosen workspace. |
| `src/cmds/init.rs` | `init_claude`, `init_gemini`, `write_mcp_server_entry` write `--workspace auto`. |
| `src/cmds/agents/adapters/*.rs` (10 adapters) | Pass `"auto"` to `render_args` instead of the current working directory. |
| `tests/e2e-sandboxed.sh` | Update assertion to expect `--workspace auto`. |
| `docs/superpowers/specs/2026-08-10-auto-mcp-workspace-design.md` | Already authored; no further changes. |

---

## Task 1: Add `resolve_auto_workspace` helper in `src/state.rs`

**Files:**
- Modify: `src/state.rs`
- Test: `src/state.rs` (in-tree `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub fn resolve_auto_workspace() -> Result<PathBuf, LainError>`.

- [ ] **Step 1: Write the failing tests**

In `src/state.rs`, inside the existing `#[cfg(test)] mod tests` block (after the existing tests), add:

```rust
    #[test]
    fn resolve_auto_workspace_finds_repo_root() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tmp("auto-root");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git2::Repository::init(&repo).unwrap();

        let cwd = std::env::current_dir().unwrap();
        let expected = std::fs::canonicalize(&repo).unwrap();

        let _restore = DirGuard::new(cwd);
        std::env::set_current_dir(&repo).unwrap();
        let resolved = Projects::resolve_auto_workspace().expect("resolve");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn resolve_auto_workspace_walks_up_to_repo_root() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tmp("auto-subdir");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let repo = dir.join("repo");
        let sub = repo.join("a/b/c");
        std::fs::create_dir_all(&sub).unwrap();
        git2::Repository::init(&repo).unwrap();

        let cwd = std::env::current_dir().unwrap();
        let expected = std::fs::canonicalize(&repo).unwrap();

        let _restore = DirGuard::new(cwd);
        std::env::set_current_dir(&sub).unwrap();
        let resolved = Projects::resolve_auto_workspace().expect("resolve");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn resolve_auto_workspace_errors_outside_repo() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tmp("auto-none");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let outside = dir.join("no-repo");
        std::fs::create_dir_all(&outside).unwrap();

        let cwd = std::env::current_dir().unwrap();
        let _restore = DirGuard::new(cwd);
        std::env::set_current_dir(&outside).unwrap();
        let err = Projects::resolve_auto_workspace().unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("--workspace auto"),
            "error should mention --workspace auto, got: {msg}"
        );
    }

    #[test]
    fn resolve_auto_workspace_rejects_bare_repo() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tmp("auto-bare");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let bare = dir.join("bare.git");
        std::fs::create_dir_all(&bare).unwrap();
        let repo = git2::Repository::init_bare(&bare).unwrap();
        let _ = repo; // silence unused
        let cwd = std::env::current_dir().unwrap();
        let _restore = DirGuard::new(cwd);
        std::env::set_current_dir(&bare).unwrap();
        let err = Projects::resolve_auto_workspace().unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("bare") || msg.contains("--workspace auto"),
            "error should mention bare repo or --workspace auto, got: {msg}"
        );
    }
```

Also add a small RAII helper at the top of the same `mod tests` block to save/restore cwd:

```rust
    struct DirGuard(PathBuf);
    impl DirGuard {
        fn new(p: PathBuf) -> Self { Self(p) }
    }
    #[allow(dead_code)]
    impl Drop for DirGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }
```

The `tmp` helper and `TEST_LOCK` already exist in the same `mod tests` block.

- [ ] **Step 2: Run the tests to confirm they fail**

Run:
```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --lib state::tests::resolve_auto_workspace
```

Expected: compile error `no function named `resolve_auto_workspace` found`.

- [ ] **Step 3: Add the implementation**

In `src/state.rs`, add the following to the `impl Projects` block (next to `resolve_workspace`):

```rust
    /// Resolve the workspace when the user passed `--workspace auto`.
    ///
    /// Walks up from the current working directory to find the nearest
    /// enclosing git repository and returns its workdir. Returns a clear
    /// user-facing error when no repository is found.
    pub fn resolve_auto_workspace() -> Result<PathBuf, LainError> {
        let repo = git2::Repository::discover(".").map_err(|e| {
            LainError::Workspace(format!(
                "--workspace auto requires a git repository, but none was found from {}: {e}. \
                 Pass an explicit --workspace <path> or run inside a git repo.",
                std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "<unknown cwd>".into())
            ))
        })?;
        let path = repo.workdir().ok_or_else(|| {
            LainError::Workspace(
                "--workspace auto: bare repositories are not supported. \
                 Pass an explicit --workspace <path>."
                    .to_string(),
            )
        })?;
        Ok(path.to_path_buf())
    }
```

Make sure `LainError` has a `Workspace(String)` variant or reuse an existing variant. If only `LainError::Io`, `LainError::Config`, etc. exist, add a new variant:

```rust
#[derive(Debug, thiserror::Error)]
pub enum LainError {
    // ... existing variants ...
    #[error("{0}")]
    Workspace(String),
}
```

(If `LainError` is defined elsewhere, add the variant there instead.)

- [ ] **Step 4: Run the tests to confirm they pass**

Run:
```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --lib state::tests::resolve_auto_workspace
```

Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/state.rs
git commit -m "feat: resolve_auto_workspace discovers git root from cwd"
```

---

## Task 2: Resolve `--workspace auto` in `main` and log the result

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `Projects::resolve_auto_workspace()` from Task 1.
- Produces: `args.workspace` mutated to a real path before any subcommand dispatch.

- [ ] **Step 1: Write the failing integration test**

Add a new test file `tests/auto_workspace.rs`:

```rust
//! Verifies that `--workspace auto` resolves to the git repo discovered
//! from the current working directory.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn lain_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lain"))
}

#[test]
fn auto_workspace_resolves_to_git_root() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&repo)
        .status()
        .expect("git init");
    assert!(status.success());

    let mut child = Command::new(lain_bin())
        .args(["--workspace", "auto", "--transport", "stdio", "--log-level", "info"])
        .env("LAIN_PORT", "19999")
        .current_dir(&repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lain");

    let mut stderr = String::new();
    let start = Instant::now();
    let deadline = Duration::from_secs(20);
    loop {
        if let Some(mut pipe) = child.stderr.take() {
            use std::io::Read;
            let mut buf = [0u8; 4096];
            match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    stderr.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if stderr.contains("Serving repo") {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        if start.elapsed() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("timeout waiting for Serving repo (stderr so far: {stderr})");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        stderr.contains("Serving repo"),
        "stderr should advertise the resolved workspace; got: {stderr}"
    );
    assert!(
        stderr.contains(repo.to_str().unwrap()),
        "stderr should include the resolved path; got: {stderr}"
    );
}
```

- [ ] **Step 2: Run the test to confirm it fails**

Run:
```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --test auto_workspace
```

Expected: fails because `--workspace auto` is not yet handled.

- [ ] **Step 3: Resolve the sentinel in `main`**

In `src/main.rs`, locate the function that processes CLI args (the `fn main` or its dispatcher). Insert the following block immediately after `args` are parsed and before any subcommand dispatch (around the call to `resolve_workspace_path`):

```rust
    if args.workspace.as_os_str() == "auto" {
        args.workspace = lain::state::Projects::resolve_auto_workspace()?;
    }
    tracing::info!(workspace = %args.workspace.display(), "Serving repo");
```

Make sure `lain::state` is already imported (`use lain::state::Projects;`).

- [ ] **Step 4: Run the test to confirm it passes**

Run:
```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --test auto_workspace
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs tests/auto_workspace.rs
git commit -m "feat: resolve --workspace auto at startup and log it"
```

---

## Task 3: Update `lain init` to write `--workspace auto`

**Files:**
- Modify: `src/cmds/init.rs` (`init_claude`, `init_gemini`, `write_mcp_server_entry`)

**Interfaces:**
- Produces: every agent's MCP args begin with `"--workspace", "auto"`.

- [ ] **Step 1: Write the failing test**

Add the following test to `src/cmds/init.rs` (extend the existing `#[cfg(test)]` block or add a new one):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_claude_writes_workspace_auto() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        // git repo required by init pre-flight
        Command::new("git").args(["init", "--quiet"]).current_dir(&workspace).status().unwrap();

        let claude_dir = home.join(".claude");
        let settings = claude_dir.join("settings.json");
        let lain_md = claude_dir.join("LAIN.md");
        init_claude(&workspace, None, "stdio", 0, true, &claude_dir, &settings, &lain_md).unwrap();

        let body = std::fs::read_to_string(&settings).unwrap();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        let args = json.pointer("/mcpServers/lain/args").unwrap().as_array().unwrap();
        let slice: Vec<String> = args.iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert!(
            slice.windows(2).any(|w| w == ["--workspace", "auto"]),
            "expected --workspace auto in args, got: {slice:?}"
        );
    }
}
```

Add `use std::process::Command;` at the top of the module if not already present.

- [ ] **Step 2: Run the test to confirm it fails**

Run:
```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --lib cmds::init::tests
```

Expected: fails because `init_claude` still writes the absolute path.

- [ ] **Step 3: Update `init_claude`**

In `src/cmds/init.rs::init_claude`, change:

```rust
        let mut args = vec![
            "--workspace".to_string(),
            workspace.to_string_lossy().to_string(),
            "--transport".to_string(),
            transport.to_string(),
        ];
```

to:

```rust
        let mut args = vec![
            "--workspace".to_string(),
            "auto".to_string(),
            "--transport".to_string(),
            transport.to_string(),
        ];
```

- [ ] **Step 4: Update `init_gemini`**

Apply the same change in `init_gemini` (it has the identical `args` vec construction).

- [ ] **Step 5: Update `write_mcp_server_entry`**

Apply the same change in `write_mcp_server_entry` (used by cursor, windsurf, cline, kimi, etc.).

- [ ] **Step 6: Run the test to confirm it passes**

Run:
```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --lib cmds::init::tests
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/cmds/init.rs
git commit -m "feat(init): write --workspace auto in agent configs"
```

---

## Task 4: Update `lain agents install` to write `--workspace auto`

**Files:**
- Modify: `src/cmds/agents/adapters/{omp,gemini,cursor,continue_dev,codex,cline,claude,antigravity,kimi}.rs`

**Interfaces:**
- Produces: `render_args` substitutes `{{workspace}}` with `"auto"`.

- [ ] **Step 1: Update all adapters**

In each of the nine adapter files, replace:

```rust
        let workspace = std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
```

with:

```rust
        let workspace = "auto".to_string();
```

For `kimi.rs`, the same pattern exists at line 19. Replace it too.

(The render_args function itself does not change; it still substitutes `{{workspace}}` with whatever string we pass.)

- [ ] **Step 2: Run the existing adapter test to verify**

Run:
```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --lib cmds::agents
```

Expected: existing tests pass (the `render_args_substitutes_workspace` test continues to pass because the function signature is unchanged).

- [ ] **Step 3: Commit**

```bash
git add src/cmds/agents/adapters
git commit -m "feat(agents install): write --workspace auto in MCP configs"
```

---

## Task 5: Update `tests/e2e-sandboxed.sh` and the running log line

**Files:**
- Modify: `tests/e2e-sandboxed.sh`

- [ ] **Step 1: Update the assertion**

In `tests/e2e-sandboxed.sh`, find the assertion that checks the MCP args contain the fake workspace path. It currently reads (approximately):

```bash
assert '--workspace' in args and '$FAKE_WORKSPACE' in args, f'unexpected args: {args!r}'
```

Replace it with:

```bash
assert '--workspace' in args and 'auto' in args, f'unexpected args: {args!r}'
```

(If the assertion is structured differently, ensure the script still treats `auto` as the expected value when `--workspace auto` is installed.)

- [ ] **Step 2: Run the sandboxed test if available**

```bash
bash tests/e2e-sandboxed.sh
```

Expected: the `lain init` step now passes because the args include `--workspace auto`.

- [ ] **Step 3: Commit**

```bash
git add tests/e2e-sandboxed.sh
git commit -m "test(e2e): expect --workspace auto in installed MCP args"
```

---

## Task 6: Full test sweep and manual smoke check

- [ ] **Step 1: Run the full library test suite**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --lib
```

Expected: 465 + new tests pass, 0 failures.

- [ ] **Step 2: Run the new integration test**

```bash
cargo test --test auto_workspace
```

Expected: PASS.

- [ ] **Step 3: Manual smoke check**

In a real git repo (e.g. `~/monitor/monitor_dm_system`):

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo run -- --workspace auto --transport stdio --log-level info
```

Expected: stderr contains `Serving repo /home/.../monitor_dm_system`.

- [ ] **Step 4: Commit any final tweaks**

If adjustments were needed, commit them with a `chore:` or `fix:` message.

---

## Out of Scope

- Removing `lain init` entirely.
- Using the active project registry to drive the MCP server.
- Windows-specific wrapper scripts.
- Bare git repository support.
