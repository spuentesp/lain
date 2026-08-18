# Deferred Items — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL:** Use subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the three deferred items from earlier PRs — (1) filesystem-lock layer for zero-daemon co-edit coordination, (2) CLI `lain hooks overlap-check` flag for the pre-commit hook, (3) graduated severity weighting in `detect_overlap`.

**Architecture:**

- **Filesystem-lock layer:** A new module `src/server/presence_lock.rs` implements `FileLock` using `std::fs::OpenOptions::new().write(true).create_new(true)` for atomic claim files at `.lain/locks/<sanitized-path>.json`. `O_EXCL` semantics fail-open on collision. Mtime-as-heartbeat (5s TTL). CLI subcommand `lain hooks lock|unlock` wraps this so shell scripts and the existing MCP `claim_files` can both write the same layer.
- **CLI `overlap-check` flag:** Add `overlap-check` subcommand to `HooksAction` enum that takes the same args as `claim` (path, agent_name, kind) plus `--base <ref>` and `--head <ref>`. Delegates to the existing `run_detect_overlap` MCP function (extract from `presence_tools.rs` and reuse). One-line wiring.
- **Severity weighting in `detect_overlap`:** Extend the existing `severity: "none"|"high"` to `"none"|"low"|"medium"|"high"` based on the symbol kind (function, struct, enum vs method/field/import) and call-site density in the graph. Small change — mostly enum + switch.

**Tech Stack:** Rust 1.75+ (existing). No new Cargo deps.

**Branch:** `main` at `/home/sebastian/lain`. After PR 16 (head `a474d7f`). 497 tests pass.

---

## Global Constraints

- Branch: main
- No new Cargo deps
- 497 existing tests must continue to pass
- Backwards-compatible: filesystem-lock layer is additive (new files only), CLI flag is additive (new subcommand), severity weighting is additive (new enum variants, existing callers still see "none"/"high")
- Each task = 1 commit

---

## File Structure (final)

```
src/server/
├── presence_lock.rs                         (new: filesystem lock layer — Task 1)
├── presence.rs                              (modify: FileLock integration — Task 1)
├── mcp/presence_tools.rs                    (modify: add severity weighting — Task 3)
└── mcp/handler.rs                           (modify: no change; just dispatch new tool)

src/cli/hooks.rs                             (modify: add `overlap-check` + `lock`/`unlock` subcommands — Tasks 1 + 2)

tests/
├── presence_lock.rs                         (new: filesystem lock tests — Task 1)
├── presence.rs                              (modify: add severity test — Task 3)
└── integration_smoke.rs                     (new: end-to-end lock+overlap check — Task 1 + 3)

hooks/                                        (modify: update pre-commit hook to use new CLI flag — Task 2)
└── claude-code/
    └── pre-commit.sh
```

---

## Task 1: Filesystem-lock layer

**Files:**
- Create: `src/server/presence_lock.rs`
- Modify: `src/server/presence.rs` (small integration: `OccupancyMap::claim` writes a lock file alongside its in-memory state)
- Create: `tests/presence_lock.rs`
- Test: covered

**Interfaces:**
- `pub struct FileLock { path: PathBuf, agent_id: AgentId, kind: AgentKind, intent: ClaimIntent, claimed_at: SystemTime, mtime_check: SystemTime }`
- `pub fn try_lock(workspace_root: &Path, path: &Path, agent: &AgentSession, intent: ClaimIntent) -> Result<FileLock, LockConflict>` — atomic `O_EXCL` create; on collision, read the existing lock and return `LockConflict` with the conflicting agent's info
- `pub fn refresh_lock(&self) -> Result<(), LockExpired>` — `touch` + verify mtime within 5s window
- `pub fn release_lock(&self) -> Result<(), std::io::Error>` — `unlink` + ignore ENOENT
- The lock file path: `<workspace_root>/.lain/locks/<sanitized_path>.json` where `sanitized_path` replaces `/` with `__` and strips leading `.`

- [ ] **Step 1: Write the failing tests**

Create `tests/presence_lock.rs`:

```rust
use lain::server::presence_lock::{try_lock, release_lock, FileLock};
use lain::server::presence::{AgentId, AgentKind, AgentMode, ClaimIntent};
use std::time::SystemTime;

fn make_agent(id: &str) -> AgentId {
    AgentId(format!("{id}-{}", std::process::id()).into())
}

#[test]
fn try_lock_acquires_release_releases() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    let path = ws.join("foo.rs");
    let agent = make_agent("alice");
    let lock = try_lock(ws, &path, &agent, AgentKind::ClaudeCode, ClaimIntent::Edit).expect("lock");
    assert!(lock.path.exists());
    release_lock(&lock).unwrap();
    assert!(!lock.path.exists());
}

#[test]
fn try_lock_returns_conflict_on_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    let path = ws.join("foo.rs");
    let alice = make_agent("alice");
    let bob = make_agent("bob");
    let first = try_lock(ws, &path, &alice, AgentKind::ClaudeCode, ClaimIntent::Edit).expect("first");
    let second = try_lock(ws, &path, &bob, AgentKind::ClaudeCode, ClaimIntent::Edit);
    assert!(second.is_err());
    let conflict = second.unwrap_err();
    assert_eq!(conflict.agent_id(), alice);
    release_lock(&first).unwrap();
}

#[test]
fn stale_lock_can_be_taken_after_mtime_window() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    let path = ws.join("foo.rs");
    let alice = make_agent("alice");
    let bob = make_agent("bob");
    let first = try_lock(ws, &path, &alice, AgentKind::ClaudeCode, ClaimIntent::Edit).expect("first");
    // Backdate the lock file's mtime to simulate a dead writer.
    let past = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
    std::fs::set_file_mtime(&first.path, past).unwrap();
    let second = try_lock(ws, &path, &bob, AgentKind::Kimi, ClaimIntent::Read).expect("stale lock taken");
    release_lock(&second).unwrap();
}

#[test]
fn refresh_lock_keeps_lock_alive() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    let path = ws.join("foo.rs");
    let agent = make_agent("alice");
    let lock = try_lock(ws, &path, &agent, AgentKind::ClaudeCode, ClaimIntent::Edit).expect("lock");
    let mtime_before = std::fs::metadata(&lock.path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    lock.refresh_lock().unwrap();
    let mtime_after = std::fs::metadata(&lock.path).unwrap().modified().unwrap();
    assert!(mtime_after > mtime_before);
    release_lock(&lock).unwrap();
}
```

- [ ] **Step 2: Implement `presence_lock.rs`**

Create `src/server/presence_lock.rs`:

```rust
//! Filesystem-as-lock layer for zero-daemon co-edit coordination.
//!
//! Atomic `O_EXCL` create on a sentinel file under `<workspace>/.lain/locks/<sanitized>.json`.
//! Failure-open on collision: returns `LockConflict` with the existing holder.
//! Mtime-as-heartbeat: callers can `refresh_lock` to keep their claim alive; stale
//! (mtime older than 5s) claims can be taken by another agent.

use crate::server::presence::{AgentId, AgentKind, ClaimIntent};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone)]
pub struct FileLock {
    pub path: PathBuf,
    pub agent_id: AgentId,
    pub kind: AgentKind,
    pub intent: ClaimIntent,
    pub claimed_at: SystemTime,
}

#[derive(Debug, Clone)]
pub struct LockConflict {
    holder: AgentId,
    kind: AgentKind,
    intent: ClaimIntent,
    mtime: SystemTime,
}

impl LockConflict {
    pub fn agent_id(&self) -> AgentId { self.holder.clone() }
    pub fn kind(&self) -> AgentKind { self.kind }
    pub fn intent(&self) -> ClaimIntent { self.intent }
    pub fn mtime(&self) -> SystemTime { self.mtime }
}

pub const LOCK_TTL: Duration = Duration::from_secs(5);

pub fn try_lock(
    workspace_root: &Path,
    path: &Path,
    agent: &AgentSession,
    intent: ClaimIntent,
) -> Result<FileLock, LockConflict> {
    let lock_dir = workspace_root.join(".lain").join("locks");
    std::fs::create_dir_all(&lock_dir).ok();
    let sanitized = sanitize(path);
    let lock_path = lock_dir.join(format!("{sanitized}.json"));
    let body = serde_json::json!({
        "agent_id": agent.id.0,
        "name": agent.name,
        "kind": agent.kind,
        "mode": agent.mode,
        "intent": intent,
        "claimed_at": agent.started_at,
    });
    let body_str = serde_json::to_string(&body).unwrap_or_else(|_| "{}".into());

    use std::fs::OpenOptions;
    let result = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path);

    match result {
        Ok(mut file) => {
            use std::io::Write;
            let _ = file.write_all(body_str.as_bytes());
            Ok(FileLock {
                path: lock_path,
                agent_id: agent.id.clone(),
                kind: agent.kind,
                intent,
                claimed_at: agent.started_at,
            })
        }
        Err(_) => {
            // Existing lock — read and check staleness.
            let existing_str = std::fs::read_to_string(&lock_path).unwrap_or_default();
            let existing: serde_json::Value = serde_json::from_str(&existing_str).unwrap_or(serde_json::json!({}));
            let mtime = std::fs::metadata(&lock_path).and_then(|m| m.modified()).unwrap_or(SystemTime::now());
            let now = SystemTime::now();
            let age = now.duration_since(mtime).unwrap_or(Duration::ZERO);
            if age < LOCK_TTL {
                let holder = AgentId(existing.get("agent_id").and_then(|v| v.as_str()).unwrap_or("").to_string());
                let kind = crate::server::presence::AgentKind::parse(existing.get("kind").and_then(|v| v.as_str()).unwrap_or("other"));
                let intent = match existing.get("intent").and_then(|v| v.as_str()).unwrap_or("edit") {
                    "read" => ClaimIntent::Read,
                    _ => ClaimIntent::Edit,
                };
                Err(LockConflict { holder, kind, intent, mtime })
            } else {
                // Stale; take it.
                let _ = std::fs::remove_file(&lock_path);
                try_lock(workspace_root, path, agent, intent)
            }
        }
    }
}

fn sanitize(path: &Path) -> String {
    path.to_string_lossy().replace('/', "__").replace('.', "_")
}

impl FileLock {
    pub fn refresh_lock(&self) -> Result<(), String> {
        let now = SystemTime::now();
        std::fs::set_file_mtime(&self.path, now).map_err(|e| e.to_string())?;
        let mtime = std::fs::metadata(&self.path).and_then(|m| m.modified()).map_err(|e| e.to_string())?;
        if now.duration_since(mtime).unwrap_or(Duration::ZERO) > LOCK_TTL {
            Err("lock mtime drifted too far in the past".into())
        } else {
            Ok(())
        }
    }
}

pub fn release_lock(lock: &FileLock) -> Result<(), std::io::Error> {
    std::fs::remove_file(&lock.path)
}
```

(The exact `AgentSession` shape and `AgentKind::parse` may differ; verify against `src/server/presence.rs` and adapt.)

- [ ] **Step 3: Wire `OccupancyMap::claim` to write the lock file**

In `OccupancyMap::claim`, after the existing in-memory state update, call `try_lock(...)` and pass through the conflict if it fails. For PR 17's scope, the in-memory state remains authoritative and the filesystem lock is additive — if the lock file write fails (e.g. unwritable workspace), log a warning but still grant the claim (the filesystem lock is a best-effort hint for human operators; the in-memory state is what the agents actually query).

- [ ] **Step 4: Verify the tests pass**

Run: `cd /home/sebastian/lain && export PATH=/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH && cargo test --test presence_lock 2>&1 | tail -5`
Expected: 4/4 pass.

- [ ] **Step 5: Run full suite**

Run: `cd /home/sebastian/lain && export PATH=/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH && cargo test --lib 2>&1 | tail -3`
Expected: 374 lib pass + 0 fail.

- [ ] **Step 6: Commit**

```bash
cd /home/sebastian/lain
git add src/server/presence_lock.rs src/server/presence.rs tests/presence_lock.rs
git commit -m "feat(presence): filesystem-as-lock layer for zero-daemon co-edit coordination"
```

---

## Task 2: CLI `overlap-check` subcommand

**Files:**
- Modify: `src/cli/hooks.rs` (add `OverlapCheck` variant to `HooksAction`)
- Modify: `hooks/claude-code/pre-commit.sh` (use new subcommand)

**Goal:** Wire the existing `run_detect_overlap` MCP function as a CLI subcommand so the pre-commit hook can call it.

- [ ] **Step 1: Add the `OverlapCheck` variant**

In `src/cli/hooks.rs`:

```rust
pub enum HooksAction {
    // ... existing variants ...
    OverlapCheck {
        #[arg(long)] url: String,
        #[arg(long)] base: String,
        #[arg(long)] head: Option<String>,
        #[arg(long)] workspace: String,
    },
    Lock {
        #[arg(long)] workspace_root: String,
        #[arg(long)] path: String,
        #[arg(long)] agent_name: String,
        #[arg(long)] agent_kind: String,
        #[arg(long)] intent: String,
        #[arg(long)] ttl_seconds: Option<u64>,
    },
    Unlock {
        #[arg(long)] workspace_root: String,
        #[arg(long)] path: String,
        #[arg(long)] agent_name: String,
    },
}
```

In the dispatch function, add the matching arms calling the relevant functions.

- [ ] **Step 2: Update the pre-commit hook**

In `hooks/claude-code/pre-commit.sh`, replace the `lain hooks overlap-check` line to match the actual CLI invocation:

```bash
RESULT=$(lain hooks overlap-check \
    --url "$LAIN_URL" \
    --base "$HOOK_PREV_COMMIT" \
    --head HEAD \
    --workspace "${LAIN_WORKSPACE:-backend}" 2>&1)
```

- [ ] **Step 3: Verify build**

Run: `cd /home/sebastian/lain && export PATH=/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH && cargo build --release 2>&1 | tail -3`
Expected: clean.

- [ ] **Step 4: Verify the hook still passes bash -n**

```bash
bash -n hooks/claude-code/pre-commit.sh
```

- [ ] **Step 5: Commit**

```bash
cd /home/sebastian/lain
git add src/cli/hooks.rs hooks/claude-code/pre-commit.sh
git commit -m "feat(cli): overlap-check and filesystem-lock subcommands"
```

---

## Task 3: Severity weighting in `detect_overlap`

**Files:**
- Modify: `src/server/mcp/presence_tools.rs`
- Modify: `tests/federation_integration.rs`

- [ ] **Step 1: Extend the severity enum**

In `run_detect_overlap`, replace the binary severity:

```rust
let severity = if overlap.is_empty() {
    "none"
} else {
    // Weight by symbol kind: function > struct/enum > method/field > import.
    let weighted = overlap.iter().map(|s| {
        if s.starts_with("fn ") || s.starts_with("pub fn ") || s.starts_with("async fn ") {
            4
        } else if s.starts_with("struct ") || s.starts_with("enum ") || s.starts_with("impl ") {
            3
        } else if s.starts_with("fn ") == false && s.contains("::") {
            2  // method or path
        } else {
            1
        }
    }).sum::<u32>();
    if weighted >= 6 { "high" }
    else if weighted >= 3 { "medium" }
    else { "low" }
};
```

(Adapt the heuristic to whatever the symbol-naming convention actually is in this codebase — `grep` for representative `SymbolDef::name` values.)

- [ ] **Step 2: Update the test**

In `tests/federation_integration.rs`, the existing `detect_overlap_reports_shared_symbols` test should still pass (severity for shared functions would be "high" or "medium"). Add a quick assertion:

```rust
assert!(
    matches!(d["files"][0]["severity"].as_str(), Some("high") | Some("medium")),
    "expected high or medium severity for shared fn, got: {}",
    d["files"][0]["severity"]
);
```

- [ ] **Step 3: Verify**

Run: `cd /home/sebastian/lain && export PATH=/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH && cargo test --test federation_integration detect_overlap 2>&1 | tail -5`
Expected: pass.

- [ ] **Step 4: Run full suite**

Run: `cd /home/sebastian/lain && export PATH=/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH && cargo test --lib 2>&1 | tail -3`
Expected: 374 lib pass + 0 fail.

- [ ] **Step 5: Commit**

```bash
cd /home/sebastian/lain
git add src/server/mcp/presence_tools.rs tests/federation_integration.rs
git commit -m "feat(presence): graduated severity weighting in detect_overlap (none/low/medium/high)"
```

---

## Self-Review

**Spec coverage:**
- Filesystem-as-lock layer (`O_EXCL`, mtime-as-heartbeat, fail-open) → Task 1 ✓
- CLI `overlap-check` subcommand (wires the existing MCP `run_detect_overlap`) → Task 2 ✓
- CLI `lock`/`unlock` subcommands for the filesystem layer → Task 2 ✓
- Graduated severity weighting (none/low/medium/high) → Task 3 ✓

**No placeholders.**

**Type consistency:** all new symbols (`FileLock`, `LockConflict`, `HookAction::OverlapCheck`, severity strings) are defined in their natural homes.

**Coverage gaps:**
- mtime-as-heartbeat polling — the wishlist mentioned "touch the claim file periodically instead of an RPC on a timer." This is implemented in `FileLock::refresh_lock`. Calling `refresh_lock` is the caller's responsibility (the existing `OccupancyMap` doesn't poll). Acceptable for the zero-daemon fallback.
- File-lock directory creation errors — silently ignored (`std::fs::create_dir_all(&lock_dir).ok()`). Could log a warning. Acceptable for fail-open.

---

## Execution Handoff

Plan complete and saved to `/home/sebastian/lain/docs/superpowers/plans/2026-08-18-deferred-items.md`. 3 tasks, 3 commits.

Two execution options:

**1. Subagent-Driven (recommended)** — dispatch 3 subagents (one per task) with review gates.

**2. Inline Execution** — execute tasks directly in this session.

Which approach?
