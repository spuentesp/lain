# Release & Diagnostic Hygiene — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a fresh clone build, a fresh install work, `lain doctor` tell the truth, and the test suite stop writing into the developer's home directory. These are small fixes whose combined effect is that a new user's first ten minutes work.

**Architecture:** Git index surgery (Task 1), three corrections to `lain doctor` (Task 2), doc/error-message reconciliation (Task 3), test isolation via `XDG_STATE_HOME` (Task 4), and CI coverage (Task 5). No new Cargo deps, no source-breaking changes.

**Branch:** `chore/release-hygiene` off `main`.

---

## Global Constraints

- **No behavior changes to the server.** This plan touches the git index, `lain doctor`, docs, tests, and CI only.
- **Task 1 rewrites the index, not history.** No force-push, no rebase of shared branches.
- **All existing tests must pass.**

---

## Task 1: Stop committing worktrees as submodules

**Files:** `.gitignore`, git index

**Goal:** A fresh `git clone` produces a working checkout.

**Context:** Both local worktrees are committed to `main` as **gitlinks** — mode `160000`, the submodule mode — with no `.gitmodules` anywhere in the repo:

```
$ git ls-files -s .worktrees/
160000 d703f23b948c8ac588366b0f14d9927dc57bc2bf 0  .worktrees/consolidation
160000 1726a7edbb860901633beddfe27ee477c4a2322c 0  .worktrees/pr1
```

A fresh clone gets two broken submodule entries pointing at commits git cannot resolve as submodules. It also means `git status` on `main` reports `.worktrees/consolidation` as modified whenever the worktree's HEAD moves, which is constant noise — that is exactly the state `main` is in right now.

- [ ] **Step 1: Remove them from the index, keep them on disk**

```bash
git rm --cached .worktrees/consolidation .worktrees/pr1
```

`--cached` is essential — a plain `git rm` would delete live worktrees.

- [ ] **Step 2: Ignore the directory**

Add to `.gitignore`:

```
# Local git worktrees — never committed (they are checkouts, not submodules)
/.worktrees/
```

- [ ] **Step 3: Verify no `.gitmodules` was left behind** and that `git ls-files -s | grep 160000` returns nothing.

- [ ] **Step 4: Verify a clean clone**

```bash
git clone . /tmp/clone-check && cd /tmp/clone-check && cargo build
git status --short   # must be empty
```

**Verification:** Clone builds; `git status` on `main` is clean.

---

## Task 2: Make `lain doctor` answer the question it was built for

**Files:** Modify `src/cli/doctor.rs`, `src/server/mcp/handler.rs`

**Goal:** `lain doctor` correctly answers "is this the binary I think it is, and is it the one that is running?" — wishlist #6.

**Context:** The command has the right shape and three defects, one of which means it cannot answer the question in the situation that produced it. Measured on 2026-08-17:

```
$ readlink -f $(which lain)
/home/sebastian/lain/.worktrees/consolidation/target/release/lain
$ lain doctor
error: unrecognized subcommand 'doctor'
```

The `$PATH` binary predates `doctor`, so the fix for #6 is unreachable from the shell where #6 happens.

- [ ] **Step 1: Check which binary is on `$PATH`**

This is the missing check and the whole point of the command. Compare `std::env::current_exe()` against the first `lain` on `$PATH`; when they differ, emit a `[WARN]` naming both paths and both versions:

```
[WARN] $PATH lain differs from the running binary
         running : /home/sebastian/lain/target/release/lain  (0.5.0, caa0427)
         on PATH : /home/sebastian/lain/.worktrees/…/lain    (0.5.0, 3027f24)
       Hook scripts invoke `lain` from $PATH — they are using the other one.
```

This matters precisely because the hook scripts call bare `lain`, so the binary an operator tests with is not necessarily the one their hooks run.

- [ ] **Step 2: Stop reading a build-machine path**

`doctor.rs:74` locates the hook scripts with `env!("CARGO_MANIFEST_DIR")` — the source directory of the machine that *compiled* the binary, baked in at compile time:

```rust
let hook = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("hooks/claude-code/pre-edit.sh");
```

On a developer box this resolves to the source tree and passes. On any `curl | bash` install that directory does not exist on the user's machine, so **every real installation gets a hard `[FAIL]`** — and `run_doctor` returns exit 1 for a healthy install.

Resolve from the install prefix instead, in order: `$LAIN_HOME`, then the directory containing `current_exe()`, then `~/.local/lain/hooks`, then `~/.config/lain/hooks`. Report which location matched. If none exists, that is a `[WARN]` ("hooks not installed") rather than a `[FAIL]` — hooks are optional.

- [ ] **Step 3: Report the running server's build**

`/health` does not expose the git SHA, so doctor cannot compare the local binary against the server that is actually answering — the mismatch axis that bites when a server has been up for two days across three rebuilds.

`build.rs` already emits `LAIN_GIT_SHA` and `lain_git_sha()` already exists. Add `commit` to the health body (this overlaps Task 7 of `2026-08-17-federation-tool-wiring.md` — coordinate, do not duplicate), then have doctor's existing reachability check compare and report:

```
[WARN] server at http://localhost:9999 is running 3027f24; this binary is caa0427
```

- [ ] **Step 4: Add `--identity`**

Cross-referenced from `2026-08-17-multiplayer-hardening.md` Task 1, Step 3. Implement it in whichever plan lands first; the other drops the step.

- [ ] **Step 5: Test.** Extend `tests/doctor_smoke.rs`: doctor exits 0 when the hooks directory is absent, and the `$PATH` comparison fires when `current_exe()` differs from the resolved `$PATH` entry.

**Verification:**
```bash
cargo build --release && ./target/release/lain doctor    # exit 0
env -u LAIN_HOME ./target/release/lain doctor            # still exit 0, hooks → WARN not FAIL
```

---

## Task 3: Reconcile the docs and error messages with the binary

**Files:** Modify `README.md`, `src/server/nlp.rs` (or wherever the unavailable message lives), `docs/hooks.md`

**Goal:** Nothing lain prints or documents points at something that does not exist.

- [ ] **Step 1: Fix the phantom subcommand**

`semantic_search`'s unavailable path returns:

```
Error: Unavailable: Semantic search unavailable: NLP model not loaded.
Install embeddings with: lain install-embeddings
```

`lain install-embeddings` does not exist — it returns `error: unrecognized subcommand`. An agent that follows the instruction gets an error, and then has to decide whether to trust the next thing lain tells it. Point at the documented path instead: `install.sh --download-model`, or the `LAIN_EMBEDDING_MODEL` env var, or `.lain/models/`.

- [ ] **Step 2: Fix the subcommand count in the README**

The README states lain "exposes exactly five subcommands" and lists them in a table. The binary has seven — `hooks` and `doctor` were added and never documented there. Replace the fixed count with the full list, and add the two missing rows.

- [ ] **Step 3: Grep for other phantom references**

```bash
grep -rnoE 'lain [a-z-]+' README.md docs/ hooks/ | sort -u
```

Check each against `lain --help`. Fix or delete anything that does not resolve. This class of error is cheap to introduce and expensive for a new user.

- [ ] **Step 4: Add a test that pins it.** `tests/version_consistency.rs` already exists — extend it to assert every `lain <subcommand>` string appearing in `README.md` is a real clap subcommand.

**Verification:** `cargo test --test version_consistency`

---

## Task 4: Stop the test suite writing into the developer's home directory

**Files:** Modify `tests/*.rs` (or add `tests/common/mod.rs`), `src/config/mod.rs`

**Goal:** `cargo test` leaves `~/.local/lain/` untouched.

**Context:** `~/.local/lain/state/` currently holds **47 files, 46 of them named `-tmp<random>.json`** — leaked by tests. The path: tests construct a `LainServer` over a `TempDir`; `ingest/mod.rs:953` calls `state_path_for_workspace(&self.config.workspace)`; that sanitizes the temp path's stem to `-tmpXXXXXX` and writes into the real `state_dir()`, because `XDG_STATE_HOME` is unset under `cargo test`.

They accumulate forever, nothing garbage-collects them, and each contains a plaintext `session_token`.

- [ ] **Step 1: Point tests at a temp state dir**

`state_dir()` (`config/mod.rs:66`) already honors `XDG_STATE_HOME`. Add a shared test helper that sets it to a per-test `TempDir` and returns a guard, and use it in every integration test that constructs a `LainServer` — `federation_integration.rs`, `hot_reload.rs`, `hot_reload_remove.rs`, `persistence_e2e.rs`, `presence_e2e.rs`, `workspace_e2e.rs`.

Note the ordering hazard: `std::env::set_var` is process-global and `cargo test` runs tests in parallel threads. Set it once from a `std::sync::Once` in the shared helper rather than per test, or the tests will race.

- [ ] **Step 2: Add an explicit `LAIN_STATE_DIR` override**

Checked before `XDG_STATE_HOME`. It makes the intent legible in tests and gives operators a way to relocate state without moving the whole XDG root.

- [ ] **Step 3: Add a guard test.** Snapshot the entry count of the real `state_dir()` before and after the suite; fail if it grew. Cheap, and it pins the whole class.

- [ ] **Step 4: Document the cleanup for existing installs.** A one-liner in `docs/TECHNICAL.md` — these files are already on every developer's machine and nothing will remove them:

```bash
find ~/.local/lain/state -name '-tmp*.json' -delete
```

**Verification:**
```bash
ls ~/.local/lain/state | wc -l    # before
cargo test --workspace --features test-utils
ls ~/.local/lain/state | wc -l    # unchanged
```

---

## Task 5: Make CI catch these

**Files:** Modify `.github/workflows/ci.yml`

**Goal:** The checks that would have caught most of this plan run on every PR.

**Context:** CI runs `cargo test --workspace --features test-utils` and nothing else — no `clippy`, no `fmt --check`, and none of the e2e scripts, despite `tests/e2e/` holding the harness that proves the multiplayer layer works.

- [ ] **Step 1: Add `cargo fmt --all -- --check`.**

- [ ] **Step 2: Add `cargo clippy --workspace --all-targets -- -D warnings`.** Expect a first-run backlog; if it is large, land the lint with `-D warnings` scoped to new code (`--no-deps` plus a baseline commit that fixes the existing hits) rather than disabling it.

- [ ] **Step 3: Run the e2e harnesses.** `tests/e2e/multiplayer-full.sh` needs a built binary and a free port — add a job that builds release and runs it, plus `federation-tools.sh` and `multiplayer-identity.sh` from the sibling plans as they land.

- [ ] **Step 4: Add a clean-clone job.** Clone the repo fresh into a temp dir and build. This is the check that catches Task 1's gitlinks, and it is three lines.

**Verification:** CI green on a PR that deliberately reintroduces a `160000` entry — the clean-clone job must fail.

---

## Suggested order

Task 1 first — it is thirty seconds and unblocks anyone cloning. Then Task 5, so the remaining tasks land with the checks already in place. Tasks 2–4 are independent.

## Definition of done

1. `git ls-files -s | grep 160000` returns nothing; a fresh clone builds.
2. `lain doctor` exits 0 on a `curl | bash` install and warns when `$PATH` disagrees with the running binary.
3. No string lain prints or documents names a nonexistent command.
4. `cargo test` leaves `~/.local/lain/state/` unchanged.
5. CI runs fmt, clippy, the e2e harnesses, and a clean-clone build.
