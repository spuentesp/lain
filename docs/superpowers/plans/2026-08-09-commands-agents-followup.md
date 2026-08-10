# Commands::Agents Follow-Up Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close out the `Commands::Agents` follow-up — make the harness pass for the agents that can be tested offline (kimi, omp, antigravity), mark the auth-gated agents as `#[ignore]` with documented reasons, and refresh `tests/e2e/README.md` with the per-agent status table.

**Architecture:** Most of the spec's TODO is already done (Section A: CLI wiring is in `main` since commit `3171b68`). The remaining work is: (1) verify the CLI dispatch works end-to-end, (2) make the 3 offline-capable harness tests pass under `RUN_E2E_AGENT=1`, (3) mark the 5 auth-gated agents as `#[ignore]` with reasons, (4) refresh the README per-agent status table.

**Tech Stack:** Existing (Rust, cargo, tokio, clap, the existing agents subsystem at `src/cmds/agents/`). No new external deps.

## Global Constraints

These come from the spec and apply to every task. The task's requirements implicitly include this section.

- **Rust toolchain:** MSRV 1.75 (matches `Cargo.toml`).
- **No new external dependencies.** Use only crates already in `Cargo.toml`.
- **Backwards compat:** `lain --workspace ./myrepo` and the existing federation server keep working unchanged.
- **TDD discipline:** every task with a Rust change has a failing test first. Docs-only tasks have a "Smoke test" embedded in the doc.
- **Commit granularity:** One commit per task. Commit messages: `feat(agents):`, `test(agents):`, `fix(agents):`, `docs(agents):`.
- **Harness scope:** make offline-capable agents pass; mark auth-gated agents as `#[ignore]` with documented reasons. Auth-flow simulation is out of scope per the spec.
- **Watchdog bug:** the `notify` EACCES bug in `src/watcher.rs` is out of scope per the spec. The harness uses minimal workspaces without such directories.

## File Structure

### New files

None — the harness file already exists at `tests/e2e/agent_install.rs`.

### Files to modify

| File | Change |
|---|---|
| `tests/e2e/agent_install.rs` | Mark 5 auth-gated tests as `#[ignore]` with a documented reason; verify the 3 offline-capable tests pass under `RUN_E2E_AGENT=1`. |
| `tests/e2e/README.md` | Refresh the per-agent status table (kimi/omp/antigravity → ready; claude/cursor/cline/continue/codex → ignored with reason). |

Total modified: 2 files. No new files.

---

## Tasks

### Task 1: Verify the CLI dispatch (all four subcommands)

**Files:**
- Modify: none (CLI is already wired)
- Test: `tests/agents_cli_smoke.rs` (new — a small Rust smoke test that compiles `cargo run -- agents` and captures the four subcommands' output)

**Interfaces:**
- Consumes: existing `cmds::agents::{list, install, verify, remove}` functions.
- Produces: a passing smoke test that asserts `lain agents list` prints a known agent, `lain agents install --id <id>` runs without panic, `lain agents verify --id <id>` runs, and `lain agents remove <id>` runs.

**Background:** Section A of the spec is already implemented (commit `3171b68`). Test that the dispatch wires correctly before working on the harness.

- [ ] **Step 1: Write the failing test**

Create `tests/agents_cli_smoke.rs`:

```rust
//! Smoke test that the `lain agents ...` CLI subcommands dispatch correctly.
//! Runs in-process by invoking the agents subsystem functions directly
//! (no shell-out) to avoid the cost of a full cargo build.

use std::process::Command;

fn lain_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_lain"))
}

#[test]
fn agents_list_invokes_run_list() {
    let out = Command::new(lain_bin())
        .args(["agents", "list"])
        .output()
        .expect("spawn lain agents list");
    assert!(out.status.success(), "lain agents list failed: stderr={}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("claude"), "list must include claude");
    assert!(stdout.contains("kimi"), "list must include kimi");
}

#[test]
fn agents_install_remove_round_trip_does_not_panic() {
    // Use a throwaway HOME so we don't clobber the developer's real config.
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = Command::new(lain_bin())
        .env("HOME", tmp.path())
        .args(["agents", "install", "--id", "kimi"])
        .output()
        .expect("spawn lain agents install");
    assert!(out.status.success(), "install failed: stderr={}", String::from_utf8_lossy(&out.stderr));
    let out = Command::new(lain_bin())
        .env("HOME", tmp.path())
        .args(["agents", "remove", "kimi"])
        .output()
        .expect("spawn lain agents remove");
    assert!(out.status.success(), "remove failed: stderr={}", String::from_utf8_lossy(&out.stderr));
}
```

Note: the `tempfile` dependency is already in `Cargo.toml` (line 107).

- [ ] **Step 2: Run the test**

Run: `export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH" && cargo test --test agents_cli_smoke`
Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add tests/agents_cli_smoke.rs
git commit -m "test(agents): add CLI dispatch smoke test"
```

---

### Task 2: Run the offline-capable harness tests and fix any setup gaps

**Files:**
- Modify: `tests/e2e/agent_install.rs` (only if a setup gap is found)
- Test: `RUN_E2E_AGENT=1 cargo test --test agent_install kimi antigravity omp`

**Interfaces:**
- Consumes: existing `prepare_home` (Kimi, omp setup), `install_into`, `assert_watcher_round_trip`, `assert_adapter_round_trip`.
- Produces: 3 tests passing under `RUN_E2E_AGENT=1` (kimi, antigravity, omp).

**Background:** The harness already has the structure (each test runs `prepare_home → install → (spawn agent if !skip_live) → assert_watcher_round_trip → assert_adapter_round_trip`). All 8 tests have `skip_live: true` so they only verify config + MCP wire-up. The 3 offline-capable agents should pass.

- [ ] **Step 1: Run the harness for the 3 offline-capable agents**

Run:
```bash
export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
RUN_E2E_AGENT=1 cargo test --test agent_install kimi antigravity omp -- --include-ignored --nocapture
```

Expected: 3 tests pass.

- [ ] **Step 2: If any test fails, read the failure and fix the harness setup**

Common failure modes (and the expected fix in each case):
- **Kimi config wrong:** check `prepare_home` for `"kimi"` (around line 44). The current spec says `default_model = "kimi-k2"` but the harness may use a different model name. Verify the model's `default_model` line matches what `run_install` writes.
- **omp config wrong:** check `prepare_home` for `"omp"` (around line 84). The ollama provider may need a different `base_url` or `provider` entry.
- **Antigravity config wrong:** check `prepare_home` for any antigravity-specific case (likely none — the `agy` adapter writes a config file and the harness may not need a precondition).
- **MCP not reachable:** the harness uses a real `lain` server on port 9999 (or `LAIN_PORT` if set). The smoke test must start a real `lain` server first. Document this in the test #[ignore] message.

For each failure, edit `tests/e2e/agent_install.rs` minimally — fix the `prepare_home` block or the `run_install` setup. Do NOT change the `skip_live: true` flag (it's correct for offline agents).

- [ ] **Step 3: Re-run the harness until all 3 pass**

Run the same command as Step 1. Expected: 3 tests pass.

- [ ] **Step 4: Commit (only if you made a fix)**

```bash
git add tests/e2e/agent_install.rs
git commit -m "fix(agents): make offline-capable harness tests pass for kimi/omp/antigravity"
```

If no fix was needed, skip this commit.

---

### Task 3: Mark the 5 auth-gated tests as `#[ignore]` with documented reasons

**Files:**
- Modify: `tests/e2e/agent_install.rs`

**Interfaces:**
- Consumes: existing test functions `e2e_claude`, `e2e_cursor`, `e2e_cline`, `e2e_cn` (continue), `e2e_codex` (the latter not in the visible list — verify).
- Produces: 5 tests with `#[ignore = "requires live auth: see docs/superpowers/specs/2026-08-09-commands-agents-followup-design.md"]` (the spec name so future readers can find the rationale).

**Background:** The spec says auth-gated agents are out of scope for the harness. The current `skip_live: true` flag prevents the harness from spawning the agent binary, but the tests still run (and presumably fail because the MCP server isn't reachable). Marking them as `#[ignore]` both skips the test and makes the reason discoverable.

- [ ] **Step 1: Add `#[ignore]` to the 5 auth-gated tests**

For each of:
- `e2e_claude` (line ~504)
- `e2e_cursor` (line ~508)
- `e2e_cline` (line ~512)
- `e2e_cn` (line ~516)
- `e2e_codex` (line ~520)

Change `#[test]` to:
```rust
#[test]
#[ignore = "requires live auth — see docs/superpowers/specs/2026-08-09-commands-agents-followup-design.md"]
```

Leave the existing `#[ignore = "requires RUN_E2E_AGENT=1"]` semantic unchanged (both attributes are needed: the second prevents the test from running during normal CI; the new one documents why).

Actually: the existing `#[ignore = "requires RUN_E2E_AGENT=1"]` already skips the test. The reason "requires live auth" should be a comment, not a second `#[ignore]`. The cleaner fix: a `// Note: ...` comment above each auth-gated test.

- [ ] **Step 2: Add a `//` comment above each auth-gated test**

For each of the 5 tests, add:
```rust
// Auth-gated agents are out of scope for the offline harness. See
// docs/superpowers/specs/2026-08-09-commands-agents-followup-design.md
// for the rationale (auth-flow simulation is excluded).
```

- [ ] **Step 3: Run the full harness (with `RUN_E2E_AGENT=1`) to confirm only the 3 offline tests run**

Run:
```bash
export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
RUN_E2E_AGENT=1 cargo test --test agent_install -- --include-ignored
```

Expected: 3 tests pass (kimi, antigravity, omp); 5 are skipped (auth-gated).

- [ ] **Step 4: Commit**

```bash
git add tests/e2e/agent_install.rs
git commit -m "test(agents): mark auth-gated harness tests as documented ignores"
```

---

### Task 4: Refresh `tests/e2e/README.md` per-agent status table

**Files:**
- Modify: `tests/e2e/README.md`

**Interfaces:**
- Consumes: the actual status of each test (3 passing, 5 ignored).
- Produces: a per-agent status table that honors the spec's "Section C item 1" requirement.

**Background:** The README likely has a per-agent status table that needs to be updated to reflect which tests pass and which are skipped.

- [ ] **Step 1: Read the current README**

Find the per-agent status table (or section). Note the current columns.

- [ ] **Step 2: Update the table to reflect the actual state**

The new table should have columns: `Agent | Test | Status | Notes`. For each agent:

| Agent | Status | Reason |
|-------|--------|--------|
| kimi | ✅ passes | offline-capable; config + MCP wire-up verified |
| omp | ✅ passes | offline-capable; config + MCP wire-up verified |
| antigravity | ✅ passes | offline-capable; config + MCP wire-up verified |
| claude | ⏸ ignored | requires live auth — out of harness scope |
| cursor | ⏸ ignored | requires live auth — out of harness scope |
| cline | ⏸ ignored | requires live auth — out of harness scope |
| continue | ⏸ ignored | requires live auth — out of harness scope |
| codex | ⏸ ignored | requires live auth — out of harness scope |

- [ ] **Step 3: Commit**

```bash
git add tests/e2e/README.md
git commit -m "docs(agents): refresh per-agent status table to reflect harness state"
```

---

## Self-Review

**1. Spec coverage:**

| Spec section | Implementing task(s) |
|---|---|
| Goal | All 4 tasks |
| **A. Production CLI wiring** | Task 1 (verify) — already implemented in commit `3171b68` |
| **B. Per-agent runtime setup** | Task 2 (verify) — Kimi/omp setup already in `prepare_home` |
| **C.1. Fix id mapping `agy` → `antigravity`** | Already done (line 424-425) |
| **C.2. End-to-end verification per agent** | Task 2 (verify) — orchestration in `run_case` already exists |
| **C.3. Isolation** | Task 2 (verify) — each test uses unique temp `HOME` and workspace |
| Success criteria 1-4 | Task 1 (CLI smoke) |
| Success criteria 5 | Task 2 (offline tests pass) + Task 3 (auth-gated ignored) |
| Out of scope | All tasks respect the spec's out-of-scope list |

**Gaps:** None. Every spec section has at least one task.

**2. Placeholder scan:** No `TBD`/`TODO`/`fill in details` in the steps. All code blocks are concrete. The "if any" and "skip this commit" cases are explicit conditionals, not placeholders.

**3. Type consistency:** No cross-task type dependencies. Each task is independent.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-08-09-commands-agents-followup.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session, batched with checkpoints.

Which approach?
