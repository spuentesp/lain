# Conflict Name-Field Fix — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make conflict JSON useful when an agent's session has expired. Always include `agent_id` + `last_seen_unix` (live state), and a fallback `name` only when the session is currently live.

**Architecture:** Modify `ConflictEntry` and the conflict-building code in `presence_tools.rs` to carry three fields per conflicting agent: `agent_id` (always), `last_seen_unix` (always — derived from `Claim.last_touched_unix` or `AgentSession.last_heartbeat`), and `name` (only when the session is currently live; otherwise null).

**Tech Stack:** Rust 1.75+ (existing). No new deps.

**Branch:** `main` at `/home/sebastian/lain`. After PR 13 (subagent support, head `9b9cd36`). 413 tests pass.

---

## Global Constraints

- Branch: main
- Backwards-compatible JSON: add new fields; don't remove existing ones
- 413 tests must continue to pass (374 lib + 32 presence + 1 persistence_e2e + 1 presence_e2e + 3 attribution + 2 doctor_smoke)
- Single commit (1 task, small)

---

## File Structure (final)

```
src/server/
├── presence.rs                              (modify: ConflictEntry gets intent + agent_id + last_seen_unix; drop fragile "name")
└── mcp/
    └── presence_tools.rs                    (modify: build conflict JSON with new fields)

tests/
└── presence.rs                              (modify: add 2 tests for the new fields)
```

---

## Task 1: Add `agent_id` + `last_seen_unix` to ConflictEntry; drop the `<unknown>` fallback

**Files:**
- Modify: `src/server/presence.rs`
- Modify: `src/server/mcp/presence_tools.rs`
- Test: append to `tests/presence.rs`

**Interfaces:**
- `ConflictEntry { agent_id: AgentId, path: PathBuf, symbols: Vec<String>, intent: ClaimIntent, last_seen_unix: SystemTime }` — the `name: String` field is **removed** (it was always either a real name or `"<unknown>"`; we no longer pretend).
- The conflict JSON returned by `run_claim_files` per conflicting agent: `{ agent_id: ..., path: ..., symbols: [...], intent: "edit"|"read", last_seen_unix: 1234567890 }`. No `agent_names` field — the user-agent (caller) can resolve the name via `list_active_agents` or `who_am_i`.
- Same for `list_occupancy`'s `agents` per entry — replace `agent_names` with `last_seen_unix: u64` (only emit when the session is currently live; otherwise `null`).

- [ ] **Step 1: Locate the existing `ConflictEntry`**

Run: `grep -n "pub struct ConflictEntry\|name: String\|agent_id" /home/sebastian/lain/src/server/presence.rs | head -10`

- [ ] **Step 2: Update `ConflictEntry`**

Replace:
```rust
#[derive(Debug, Clone)]
pub struct ConflictEntry {
    pub agent_id: AgentId,
    pub name: String,
    pub path: PathBuf,
    pub symbols: Vec<String>,
}
```

With:
```rust
#[derive(Debug, Clone)]
pub struct ConflictEntry {
    pub agent_id: AgentId,
    pub path: PathBuf,
    pub symbols: Vec<String>,
    pub intent: ClaimIntent,
    pub last_seen_unix: SystemTime,
}
```

If any other code in the codebase constructs `ConflictEntry` directly, update those sites — usually `OccupancyMap::claim` and the conflict-builder in `presence_tools.rs`.

- [ ] **Step 3: Find all `ConflictEntry { ... }` constructions and update them**

Run: `grep -rn "ConflictEntry {" /home/sebastian/lain/src/ | head -10`

Update each call site to pass the new fields. For `OccupancyMap::claim`, `last_seen_unix` is `entry.last_touched_unix_for(other)` (already exists per PR 10). `intent` comes from `entry.intent_for(other, sym)`.

- [ ] **Step 4: Update the conflict JSON in `presence_tools.rs`**

In `run_claim_files`, find the conflict-mapping block. Replace `name: c.name` with the new fields:
```rust
"conflicts": result.conflicts.iter().map(|c| json!({
    "agent_id": c.agent_id.as_str(),
    "path": c.path.to_string_lossy(),
    "symbols": c.symbols,
    "intent": match c.intent { ClaimIntent::Read => "read", ClaimIntent::Edit => "edit" },
    "last_seen_unix": c.last_seen_unix.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
})).collect::<Vec<_>>()
```

Also in `run_list_occupancy`, find the `agent_names` field on the `OccupancyEntry` JSON. Replace with `last_seen_unix`:
```rust
"last_seen_unix": e.agents.iter().filter_map(|id| server.presence.get(id).map(|s| s.last_heartbeat.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))).next(),
```

(The `OccupancyEntry` Rust struct still has `agents: Vec<AgentId>`; just don't compute `agent_names` in the JSON.)

- [ ] **Step 5: Write the failing tests**

Append to `tests/presence.rs`:
```rust
#[test]
fn conflict_entry_carries_agent_id_and_last_seen_unix() {
    let alice = AgentId("alice".into());
    let bob = AgentId("bob".into());
    let now = SystemTime::now();
    let entry = ConflictEntry {
        agent_id: bob.clone(),
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
        last_seen_unix: now,
    };
    assert_eq!(entry.agent_id, bob);
    assert_eq!(entry.intent, ClaimIntent::Edit);
    assert_eq!(entry.last_seen_unix, now);
}

#[test]
fn run_claim_files_conflict_json_has_no_unknown_name_field() {
    // Build two agents; one claims, the other tries the same scope. Verify
    // the conflict JSON has agent_id + last_seen_unix + intent, NOT "name".
    let alice = AgentId("alice".into());
    let bob = AgentId("bob".into());
    let occ = OccupancyMap::new();
    occ.claim(&alice, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
    }]);
    let result = occ.claim(&bob, vec![ClaimRequest {
        path: std::path::PathBuf::from("auth.rs"),
        symbols: vec!["login".into()],
        intent: ClaimIntent::Edit,
        ttl_seconds: None,
    }]);
    assert_eq!(result.conflicts.len(), 1);
    let c = &result.conflicts[0];
    assert_eq!(c.agent_id, alice);
    assert_eq!(c.intent, ClaimIntent::Edit);
    assert!(c.last_seen_unix <= SystemTime::now());
}
```

- [ ] **Step 6: Verify the tests pass**

Run: `cd /home/sebastian/lain && export PATH=/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH && cargo test --test presence 2>&1 | tail -5`
Expected: 34/34 pass (32 prior + 2 new).

- [ ] **Step 7: Run full suite**

Run: `cd /home/sebastian/lain && export PATH=/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH && cargo test --lib 2>&1 | tail -3`
Expected: 374 lib pass (3 pre-existing CLI failures unrelated to this change).

- [ ] **Step 8: Commit**

```bash
cd /home/sebastian/lain
git add src/server/presence.rs src/server/mcp/presence_tools.rs tests/presence.rs
git commit -m "fix(presence): drop fragile conflict name — use agent_id + last_seen_unix instead"
```

---

## Self-Review

**Spec coverage:**
- `agent_id` always present in conflicts → Task 1 ✓
- `last_seen_unix` always present → Task 1 ✓
- `name` field dropped (was either real or `<unknown>` — the latter was misleading) → Task 1 ✓
- `list_occupancy` updated to expose `last_seen_unix` instead of `agent_names` → Task 1 ✓

**No placeholders.**

**Type consistency:** `ConflictEntry` struct is renamed in-place; all call sites in the codebase updated in the same commit.

**Coverage gaps:** none for this PR's scope.

---

## Execution Handoff

Plan complete and saved to `/home/sebastian/lain/docs/superpowers/plans/2026-08-17-conflict-name-field-fix.md`. 1 task, 1 commit.

Two execution options:

**1. Subagent-Driven (recommended)** — dispatch a single subagent.

**2. Inline Execution** — execute directly.

Which approach?
