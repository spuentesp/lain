# Agent End-to-End DST-Style Test Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One Rust integration test binary that drives every installed agent binary through the same scripted scenario, asserts the same invariants, and exercises the file-watcher path. The harness is gated on `RUN_E2E_AGENT=1` so the default `cargo test` run stays fast.

**Architecture:** A single `tests/e2e/agent_install.rs` integration test binary with one `#[ignore]`-marked `#[test]` per agent, all calling a shared `run_case(&AgentCase)` helper. Each case runs `lain agents install --scope user <id>` in a temp `HOME`, spawns the real agent binary with the test prompt on stdin, captures stdout+stderr up to a 90 s timeout, applies the same five invariants, exercises the file-watcher round-trip, and re-runs the install loop on a fresh temp HOME to assert the adapter round-trip. The same scenario script and the same five invariants run for every agent.

**Tech Stack:** Rust 2021, `std::process::Command` (no new deps), `tempfile` (already a dev-dep), `assert_cmd` will be added as a dev-dep only if the stdlib `Command` proves insufficient (the existing `tests/agents_install.rs` uses raw `std::process::Command` and that is the pattern the harness follows). No production-code changes.

## Global Constraints

- One new file: `tests/e2e/agent_install.rs`. No other source files are touched.
- No production-code changes. The harness lives in `tests/` and uses only public APIs.
- The test is `#[ignore]`-marked by default and runs only with `RUN_E2E_AGENT=1` in the env.
- The harness depends on the live HTTP singleton at `LAIN_PORT` (default 9999). The `LAIN_URL` env var can override the URL.
- For non-auth-gated agents (Kimi, agy, omp), the test passes and reports `Operational` in the captured output.
- For auth-gated agents (Claude, Cursor, Cline, Continue, Codex), the test reports `auth-gated: skipped inner assertions` and the CI run is green except for those rows.
- The watcher round-trip asserts that a trigger file written inside the watched workspace shows up in `get_health` within 5 s.
- The adapter round-trip asserts that the install loop produces a valid JSON config with `mcpServers.lain` and the expected command/URL.
- No regressions: `cargo test --all-targets` remains green (the new test binary builds; tests skip without the env var).
- No new production dependencies. `assert_cmd` may be added as a dev-dep only if the stdlib `Command` proves insufficient.
- All work lands on `main`. This is a host-managed linked worktree already on `main`.
- No git commit, push, reset, or other mutations without explicit user authorization.
- The pre-existing `tests/agents_install.rs` and `tests/dual_instance.rs` continue to pass; the new harness is additive.

---

## File Map

- **Create:** `tests/e2e/agent_install.rs` — the new integration test binary.
- **Create:** `tests/e2e/README.md` (or extend the existing one) — a short note explaining `RUN_E2E_AGENT=1` and the per-agent auth gating. Optional, but recommended.
- **Modify (only if stdlib `Command` is insufficient):** `Cargo.toml` — add `assert_cmd = "2.0"` to `[dev-dependencies]`. This is conditional and is decided in Task 1.

---

### Task 1: Scaffold `tests/e2e/agent_install.rs` with `AgentCase` + a single test, plus the `ChildGuard`-style server guard

**Files:**
- Create: `tests/e2e/agent_install.rs`

**Interfaces:**
- Consumes: `CARGO_BIN_EXE_lain` env (provided by Cargo).
- Produces:
  - `pub struct AgentCase { pub id: &'static str, pub binary: &'static str, pub run_args: &'static [String], pub requires_auth: bool, pub workspace: &'static Path }`
  - `static AGENT_CASES: &[AgentCase] = &[ ... ];` (empty or with one placeholder entry in this task; populated in Task 2)
  - `fn run_case(case: &AgentCase) { ... }` — the per-agent scenario runner.
  - `struct ChildGuard(std::process::Child);` with `Drop` that `kill()`s and `wait()`s.
  - `fn lain_bin() -> PathBuf` — `PathBuf::from(env!("CARGO_BIN_EXE_lain"))`.
  - `fn pick_port() -> u16` — bind `127.0.0.1:0` and return the chosen port.

- [ ] **Step 1: Write the failing test skeleton**

Create `tests/e2e/agent_install.rs` with one test that fails because `AGENT_CASES` is empty:

```rust
//! End-to-end DST-style harness: drives every installed agent binary
//! through the same scripted scenario and asserts the same invariants.
//! Gated on `RUN_E2E_AGENT=1`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn lain_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lain"))
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
}

fn pick_port() -> u16 {
    use std::net::TcpListener;
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let p = l.local_addr().expect("addr").port();
    drop(l);
    p
}

pub struct AgentCase {
    pub id: &'static str,
    pub binary: &'static str,
    pub run_args: &'static [String],
    pub requires_auth: bool,
    pub workspace: &'static Path,
}

static AGENT_CASES: &[AgentCase] = &[];

fn run_case(_case: &AgentCase) {
    // populated in task 2
}

#[test]
#[ignore = "requires RUN_E2E_AGENT=1"]
fn e2e_kimi() { run_case(&AGENT_CASES[0]) }
```

- [ ] **Step 2: Run the test, confirm it fails**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --test agent_install -- --nocapture
```

Expected: FAIL with `index out of bounds` (the test binary builds; the panic is from `AGENT_CASES[0]` on an empty slice).

- [ ] **Step 3: Add a no-op `run_case` body**

```rust
fn run_case(_case: &AgentCase) {
    // No-op placeholder; populated in task 2.
}
```

- [ ] **Step 4: Run, confirm the test compiles but is `#[ignore]`-d**

```bash
cargo test --test agent_install -- --include-ignored
```

Expected: PASS (the test compiles; the placeholder body is a no-op; `--include-ignored` runs the ignored test).

- [ ] **Step 5: Stage but do not commit**

```bash
git add tests/e2e/agent_install.rs
```

---

### Task 2: `run_case` body — install, spawn agent, capture output, run the five invariants

**Files:**
- Modify: `tests/e2e/agent_install.rs`

**Interfaces:**
- Consumes: `AgentCase` from Task 1, `lain_bin()` from Task 1, `pick_port()` from Task 1, `tempfile::tempdir()`.
- Produces:
  - `fn run_case(case: &AgentCase) -> Result<(), String>` — runs the per-agent scenario and returns `Ok(())` on success or `Err(msg)` on any assertion failure.
  - The five invariants are encapsulated in a single `assert_case_invariants(stdout: &str, stderr: &str, exit: ExitStatus, case: &AgentCase) -> Result<(), String>` helper.

- [ ] **Step 1: Write the install + spawn + capture helper**

```rust
fn install_into(case: &AgentCase, home: &Path, port: u16) -> Command {
    let xdg = home.join(".config");
    let mut c = Command::new(lain_bin());
    c.env("HOME", home)
        .env("XDG_CONFIG_HOME", xdg)
        .env("LAIN_PORT", port.to_string())
        .args(["agents", "install", "--scope", "user", case.id]);
    c
}

fn spawn_agent(case: &AgentCase, home: &Path) -> Child {
    let prompt = "list the MCP tools you have, then call get_health on the one named lain, and print both the tool list and the get_health response verbatim";
    let mut c = Command::new(case.binary);
    c.current_dir(case.workspace)
        .env("HOME", home)
        .args(case.run_args.iter().map(|s| s.as_str()).collect::<Vec<_>>())
        .arg(prompt)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c.spawn().expect("agent spawn")
}
```

- [ ] **Step 2: Write the five invariants**

```rust
fn assert_case_invariants(stdout: &str, stderr: &str, case: &AgentCase) -> Result<(), String> {
    if !case.requires_auth {
        let tool_re = regex::Regex::new(r"mcp__plugin-lain_lain__\w+").unwrap();
        if !tool_re.is_match(stdout) {
            return Err(format!("{}: tool list missing mcp__plugin-lain_lain__* (stdout head: {:.200})", case.id, stdout));
        }
        if !stdout.to_lowercase().contains("operational") {
            return Err(format!("{}: get_health body missing 'Operational' (stdout head: {:.200})", case.id, stdout));
        }
        if !stdout.contains("static_nodes") {
            return Err(format!("{}: get_health body missing 'static_nodes' (stdout head: {:.200})", case.id, stdout));
        }
    } else {
        eprintln!("[{}] auth-gated: skipped inner assertions", case.id);
    }
    if stderr.contains("error sending request") || stderr.contains("connect error") {
        return Err(format!("{}: stderr reports a fatal MCP error (stderr head: {:.200})", case.id, stderr));
    }
    Ok(())
}
```

- [ ] **Step 3: Wire the timeout + capture + asserts in `run_case`**

```rust
fn run_case(case: &AgentCase) -> Result<(), String> {
    use std::io::Read;
    let tmp = tempfile::tempdir().expect("tempdir");
    let port = pick_port();
    // Init the workspace with .git so Lain's main() does not bail.
    let init_status = Command::new("git").args(["init", "--quiet", case.workspace.to_str().unwrap()]).status().map_err(|e| e.to_string())?;
    if !init_status.success() { return Err(format!("{}: git init failed", case.id)); }
    let _ = install_into(case, tmp.path(), port).status().map_err(|e| e.to_string())?;
    let mut child = spawn_agent(case, tmp.path());
    let timeout = Duration::from_secs(90);
    let start = Instant::now();
    let mut stdout = String::new();
    let mut stderr = String::new();
    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{}: timed out after {:?}", case.id, timeout));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(format!("{}: wait error: {}", case.id, e)),
        }
    };
    child.stdout.take().unwrap().read_to_string(&mut stdout).map_err(|e| e.to_string())?;
    child.stderr.take().unwrap().read_to_string(&mut stderr).map_err(|e| e.to_string())?;
    let status = exit.map_err(|e| e.to_string())?;
    if !status.success() && !case.requires_auth {
        return Err(format!("{}: non-zero exit ({:?}); stderr: {:.200}", case.id, status, stderr));
    }
    assert_case_invariants(&stdout, &stderr, case)
}
```

- [ ] **Step 4: Add the `regex` dev-dep to `Cargo.toml`**

The harness uses the `regex` crate. Add it to `[dev-dependencies]`:

```toml
regex = "1.10"
```

(Already a regular dep at the top of `Cargo.toml`; this is the same version, but adding it under `[dev-dependencies]` keeps Cargo's per-target dependency model clean even though the regular dep is already there. Skip this step if `cargo test --test agent_install` compiles without it — i.e. if the transitive dep is already available.)

- [ ] **Step 5: Update `run_case` callers to assert `Ok(())`**

```rust
fn run_case(case: &AgentCase) { assert_eq!(super::run_case(case), Ok(()), "agent case {} failed", case.id); }
```

(The wrapper panics on `Err(msg)`; the assertion message includes the `msg` from `Err`.)

- [ ] **Step 6: Add Kimi and `agy` cases to the static list**

```rust
use std::path::Path;

static LANGOSTINO: &Path = Path::new("/home/sebastian/orca/workspaces/lain/langostino");

static AGENT_CASES: &[AgentCase] = &[
    AgentCase { id: "kimi",  binary: "kimi",  run_args: &["--yolo".to_string(), "--print-timeout".to_string(), "60s".to_string()], requires_auth: false, workspace: LANGOSTINO },
    AgentCase { id: "agy",   binary: "agy",   run_args: &["--dangerously-skip-permissions".to_string(), "--print-timeout".to_string(), "60s".to_string()], requires_auth: false, workspace: LANGOSTINO },
];
```

(Adjust the path constants if your worktree lives at a different absolute path. The `LANGOSTINO` constant must be a real path on the test host; the harness is opt-in via `RUN_E2E_AGENT=1` so a wrong path only affects the run that explicitly opts in.)

- [ ] **Step 7: Add `e2e_agy` and `e2e_kimi` tests**

```rust
#[test]
#[ignore = "requires RUN_E2E_AGENT=1"]
fn e2e_agy() { run_case(&AGENT_CASES[0]) }

#[test]
#[ignore = "requires RUN_E2E_AGENT=1"]
fn e2e_kimi() { run_case(&AGENT_CASES[1]) }
```

- [ ] **Step 8: Run with `RUN_E2E_AGENT=1` and confirm both pass**

```bash
export PATH="/home/sebastian/.local/bin:/home/sebastian/.nvm/versions/node/v24.14.1/bin:$HOME/.bun/bin:$PATH"
RUN_E2E_AGENT=1 cargo test --test agent_install -- --include-ignored --nocapture
```

Expected: both `e2e_agy` and `e2e_kimi` PASS. The `Operational` line and the `static_nodes` line are both present in Kimi and agy's output.

- [ ] **Step 9: Stage but do not commit**

```bash
git add tests/e2e/agent_install.rs Cargo.toml Cargo.lock
```

---

### Task 3: Add the file-watcher round-trip assertion to `run_case`

**Files:**
- Modify: `tests/e2e/agent_install.rs`

**Interfaces:**
- Consumes: the existing `run_case` from Task 2; `get_health` body field `Last Enriched Commit`; a temp file inside the agent's `workspace` directory.
- Produces:
  - `fn assert_watcher_round_trip(case: &AgentCase, home: &Path, port: u16, before: &str) -> Result<(), String>` — writes a `trigger.py` into `case.workspace`, polls `get_health` for up to 5 s, asserts the body changes (i.e. the watcher picked it up).

- [ ] **Step 1: Add the watcher round-trip helper**

```rust
fn get_health_json(port: u16) -> Option<String> {
    use std::io::Write;
    let mut child = Command::new(lain_bin())
        .args(["--workspace", "/tmp/lain-agent-test",
               "--transport", "http", "--port", &port.to_string(),
               "--mode", "sidecar",
               "--embedding-model", "/home/sebastian/.local/lain/models/all-MiniLM-L6-v2.onnx"])
        .env("LAIN_PORT", port.to_string())
        .env("LAIN_OWNER_URL", format!("http://127.0.0.1:{}", port))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn().ok()?;
    // (Simplified; the real version uses a ChildGuard and the same timeout
    //  pattern as Task 2's spawn_agent helper. See the spec §Data flow.)
    let mut out = String::new(); child.stdout.take()?.read_to_string(&mut out).ok()?;
    out.lines().find(|l| l.contains("Operational") || l.contains("Last Enriched")).map(|s| s.to_string())
}
```

(Skip this helper's body in the plan — the implementer will write it from the spec's data-flow section. The key requirement is that the helper returns the body line for `get_health`.)

- [ ] **Step 2: Add the assertion call at the end of `run_case`**

```rust
let trigger = case.workspace.join("e2e_trigger.py");
let before = get_health_json(port).unwrap_or_default();
std::fs::write(&trigger, b"# lain e2e trigger\n").map_err(|e| e.to_string())?;
let deadline = Instant::now() + Duration::from_secs(5);
let mut last = before.clone();
while Instant::now() < deadline {
    if let Some(line) = get_health_json(port) {
        if line != before && line.contains("Operational") { last = line; break; }
    }
    std::thread::sleep(Duration::from_millis(500));
}
let _ = std::fs::remove_file(&trigger);
if last == before {
    return Err(format!("{}: watcher did not surface trigger file in get_health body within 5s (before/after identical)", case.id));
}
```

- [ ] **Step 3: Run with `RUN_E2E_AGENT=1` and confirm the watcher round-trip passes**

```bash
export PATH="/home/sebastian/.local/bin:/home/sebastian/.nvm/versions/node/v24.14.1/bin:$HOME/.bun/bin:$PATH"
RUN_E2E_AGENT=1 cargo test --test agent_install -- --include-ignored --nocapture
```

Expected: both `e2e_agy` and `e2e_kimi` PASS. The `e2e_trigger.py` file is created in the watched workspace and the sidecar's `get_health` body reflects the new state within 5 s.

- [ ] **Step 4: Stage but do not commit**

```bash
git add tests/e2e/agent_install.rs
```

---

### Task 4: Add the adapter round-trip assertion (one fresh install + assert config shape)

**Files:**
- Modify: `tests/e2e/agent_install.rs`

**Interfaces:**
- Consumes: the existing `run_case` from Task 2.
- Produces:
  - `fn assert_adapter_round_trip(case: &AgentCase) -> Result<(), String>` — runs `lain agents install --scope user <id>` in a fresh temp HOME, asserts the resulting config file exists, is valid JSON, contains the `mcpServers.lain` key, and points at the expected command/URL.

- [ ] **Step 1: Add the round-trip helper**

```rust
fn assert_adapter_round_trip(case: &AgentCase) -> Result<(), String> {
    let home = tempfile::tempdir().expect("tempdir");
    let port = pick_port();
    let xdg = home.path().join(".config");
    let status = Command::new(lain_bin())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", xdg)
        .env("LAIN_PORT", port.to_string())
        .args(["agents", "install", "--scope", "user", case.id])
        .status().map_err(|e| e.to_string())?;
    if !status.success() { return Err(format!("{}: install failed in round-trip", case.id)); }
    // The seven adapter manifests write different paths; the `agents list`
    // command prints the resolved path. Use that to find the config.
    let list_out = Command::new(lain_bin())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", xdg)
        .args(["agents", "list"])
        .output().map_err(|e| e.to_string())?;
    let list = String::from_utf8_lossy(&list_out.stdout);
    let line = list.lines().find(|l| l.contains(case.id)).ok_or_else(|| format!("{}: agents list missing row", case.id))?;
    // Parse the path (4th whitespace-separated field, roughly).
    let path = line.split_whitespace().last().ok_or_else(|| format!("{}: cannot parse config path", case.id))?;
    let full = home.path().join(path.trim_start_matches("~/").replace("~/", ""));
    // If the manifest uses `config_user` like `~/.foo/bar.json`, expand
    // the tilde; the simplest path is to read the dir and find the file
    // by name if the path is a glob.
    let body = std::fs::read_to_string(&full).map_err(|e| format!("{}: cannot read {}: {}", case.id, full.display(), e))?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("{}: bad json in {}: {}", case.id, full.display(), e))?;
    let lain = json.pointer("/mcpServers/lain").ok_or_else(|| format!("{}: mcpServers.lain missing", case.id))?;
    if !case.requires_auth {
        let _cmd = lain.get("command").or_else(|| lain.get("url")).ok_or_else(|| format!("{}: no command or url", case.id))?;
    }
    Ok(())
}
```

(If the config path uses `~`, expand with `dirs::home_dir()` or with the `HOME` env you just set. The implementer may simplify by using `std::env::var("HOME")` + `Path::join`.)

- [ ] **Step 2: Add the assertion call at the end of `run_case`**

```rust
assert_adapter_round_trip(case)?;
```

- [ ] **Step 3: Add `serde_json` to `[dev-dependencies]` if not already there**

`Cargo.toml:38` already lists `serde_json = "1.0"` as a regular dep. If `cargo test --test agent_install` compiles without an additional dev-dep, skip this step.

- [ ] **Step 4: Run with `RUN_E2E_AGENT=1` and confirm both pass**

```bash
export PATH="/home/sebastian/.local/bin:/home/sebastian/.nvm/versions/node/v24.14.1/bin:$HOME/.bun/bin:$PATH"
RUN_E2E_AGENT=1 cargo test --test agent_install -- --include-ignored --nocapture
```

Expected: both `e2e_agy` and `e2e_kimi` PASS, with the round-trip reporting `mcpServers.lain` is present in the written config.

- [ ] **Step 5: Stage but do not commit**

```bash
git add tests/e2e/agent_install.rs
```

---

### Task 5: Add the remaining 6 agent cases (claude, cursor, cline, cn, omp, codex) — all `requires_auth = true` for this iteration

**Files:**
- Modify: `tests/e2e/agent_install.rs`

**Interfaces:**
- Consumes: the existing `run_case` from Tasks 2–4.
- Produces: six new `#[ignore = "requires RUN_E2E_AGENT=1; auth-gated"]` tests, one per agent. Each test runs `run_case` against the new row.

- [ ] **Step 1: Extend `AGENT_CASES` with the remaining six rows**

```rust
static AGENT_CASES: &[AgentCase] = &[
    AgentCase { id: "kimi",    binary: "kimi",        run_args: &["--yolo".to_string(), "--print-timeout".to_string(), "60s".to_string()],                 requires_auth: false, workspace: LANGOSTINO },
    AgentCase { id: "agy",     binary: "agy",         run_args: &["--dangerously-skip-permissions".to_string(), "--print-timeout".to_string(), "60s".to_string()], requires_auth: false, workspace: LANGOSTINO },
    AgentCase { id: "claude",  binary: "claude",      run_args: &["--dangerously-skip-permissions".to_string()],                                       requires_auth: true,  workspace: LANGOSTINO },
    AgentCase { id: "cursor",  binary: "cursor-agent", run_args: &["--print".to_string()],                                                            requires_auth: true,  workspace: LANGOSTINO },
    AgentCase { id: "cline",   binary: "cline",       run_args: &["--yolo".to_string(), "--print".to_string(), "--output-format".to_string(), "json".to_string()],   requires_auth: true,  workspace: LANGOSTINO },
    AgentCase { id: "cn",      binary: "cn",          run_args: &["-p".to_string(), "--output-format".to_string(), "json".to_string()],                   requires_auth: true,  workspace: LANGOSTINO },
    AgentCase { id: "omp",     binary: "omp",         run_args: &["-p".to_string(), "--provider".to_string(), "ollama".to_string(), "--model".to_string(), "qwen2.5:latest".to_string(), "--yolo".to_string()], requires_auth: false, workspace: LANGOSTINO },
    AgentCase { id: "codex",   binary: "codex",      run_args: &["exec".to_string(), "--yolo".to_string()],                                            requires_auth: true,  workspace: LANGOSTINO },
];
```

(omp is `requires_auth: false` because local Ollama does not need an API key. The other five need user-side sign-in.)

- [ ] **Step 2: Add six more `#[test]`s, all auth-gated**

```rust
#[test]
#[ignore = "requires RUN_E2E_AGENT=1; auth-gated"]
fn e2e_claude() { run_case(&AGENT_CASES[2]) }

#[test]
#[ignore = "requires RUN_E2E_AGENT=1; auth-gated"]
fn e2e_cursor() { run_case(&AGENT_CASES[3]) }

#[test]
#[ignore = "requires RUN_E2E_AGENT=1; auth-gated"]
fn e2e_cline() { run_case(&AGENT_CASES[4]) }

#[test]
#[ignore = "requires RUN_E2E_AGENT=1; auth-gated"]
fn e2e_cn() { run_case(&AGENT_CASES[5]) }

#[test]
#[ignore = "requires RUN_E2E_AGENT=1"]
fn e2e_omp() { run_case(&AGENT_CASES[6]) }

#[test]
#[ignore = "requires RUN_E2E_AGENT=1; auth-gated"]
fn e2e_codex() { run_case(&AGENT_CASES[7]) }
```

- [ ] **Step 3: Run with `RUN_E2E_AGENT=1` and confirm Kimi, agy, and omp pass; the others are reported as auth-gated**

```bash
export PATH="/home/sebastian/.local/bin:/home/sebastian/.nvm/versions/node/v24.14.1/bin:$HOME/.bun/bin:$PATH"
RUN_E2E_AGENT=1 cargo test --test agent_install -- --include-ignored --nocapture
```

Expected: `e2e_agy`, `e2e_kimi`, `e2e_omp` PASS. The auth-gated ones print `[id] auth-gated: skipped inner assertions` and exit 0.

- [ ] **Step 4: Stage but do not commit**

```bash
git add tests/e2e/agent_install.rs
```

---

### Task 6: Final automated verification + docs

**Files:**
- Create: `tests/e2e/README.md` (or extend the existing one)

**Interfaces:**
- Consumes: the harness from Tasks 1–5.
- Produces: a short README section that documents `RUN_E2E_AGENT=1`, the auth-gated per-agent behavior, and the watcher / adapter round-trip semantics.

- [ ] **Step 1: Run the full test matrix**

```bash
export PATH="/home/sebastian/.local/bin:/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --all-targets
RUN_E2E_AGENT=1 cargo test --test agent_install -- --include-ignored --nocapture
```

Expected: default `cargo test --all-targets` is green (the new test binary builds; tests skip without the env var). `RUN_E2E_AGENT=1` runs the per-agent tests, all of which are green except the auth-gated ones (which are reported as `auth-gated: skipped inner assertions` and exit 0).

- [ ] **Step 2: Add a short `tests/e2e/README.md` section (or extend the existing one)**

```markdown
## Agent end-to-end harness

`tests/e2e/agent_install.rs` drives every installed agent binary
through the same scripted scenario and asserts the same five
invariants. Run it with `RUN_E2E_AGENT=1`:

```bash
RUN_E2E_AGENT=1 cargo test --test agent_install -- --include-ignored --nocapture
```

Per-agent behavior:

- Kimi, agy, omp: the harness exercises the live HTTP singleton end
  to end (install + spawn + tool list + get_health + watcher round-trip).
- claude, cursor, cline, cn, codex: auth-gated. The test installs the
  config and runs the binary; the binary fails to authenticate, the
  stderr-as-fatal check catches the error, and the test reports
  `auth-gated: skipped inner assertions` so a CI run is green except
  for those rows. Once you sign in to the relevant agent, the test
  automatically picks up the working `get_health` reply.

The harness does not require the live HTTP singleton to be
re-installed; it uses `lain agents install --scope user <id>` against
a fresh temp HOME for the round-trip step.

The harness is DST-style: one fixed scenario script, per-agent run,
fixed output contract. It is not a FoundationDB-class simulator; it is
a deterministic run that produces a clear pass/fail per agent.
```

- [ ] **Step 3: Final diff inspection**

```bash
git diff --check
git status --short
git diff --stat -- tests/e2e/agent_install.rs Cargo.toml Cargo.lock tests/e2e/README.md
```

Expected: only `tests/e2e/agent_install.rs` and (if added) `tests/e2e/README.md` are modified, plus the dev-dep additions to `Cargo.toml`/`Cargo.lock`.

- [ ] **Step 4: Stage but do not commit**

```bash
git add tests/e2e/agent_install.rs tests/e2e/README.md Cargo.toml Cargo.lock
```

---

### Task 7: Live verification (single end-to-end run with `RUN_E2E_AGENT=1`)

**Files:** none.

**Interfaces:** the harness from Tasks 1–6.

- [ ] **Step 1: Run the harness against the live host**

```bash
export PATH="/home/sebastian/.local/bin:/home/sebastian/.nvm/versions/node/v24.14.1/bin:$HOME/.bun/bin:$PATH"
RUN_E2E_AGENT=1 cargo test --test agent_install -- --include-ignored --nocapture 2>&1 | tee /tmp/agent_install_e2e.log
```

Expected: `e2e_agy`, `e2e_kimi`, `e2e_omp` PASS with `Operational` and a non-zero `static_nodes`. The auth-gated ones print `auth-gated: skipped inner assertions`. The watcher round-trip fires the `e2e_trigger.py` file and sees it within 5 s. The adapter round-trip writes the config in a fresh temp HOME and asserts `mcpServers.lain` is present.

- [ ] **Step 2: Verify the live HTTP singleton is still healthy**

```bash
curl -sS -X POST http://localhost:9999/mcp -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_health","arguments":{}}}' | head -c 600
```

Expected: `Status: Operational ✅`, no new disconnect lines in `/home/sebastian/monitor/monitor_dm_system/.lain/server.log`.

- [ ] **Step 3: Do not commit or push

No `git commit`, `git push`, `git reset`, or other mutations without explicit authorization. Record the live evidence in the SDD plan workspace.
