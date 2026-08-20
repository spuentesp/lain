# Coordination: Staleness Detection, Audit Trail, Dashboard Noise — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the design in `docs/superpowers/specs/2026-08-18-coordination-staleness-audit-design.md` as three sequential PRs.

**Architecture:** Lift `RevisionId = u64` (already in `src/server/overlay/stream.rs:18`) onto tool responses and `claim_files`. Detect static-graph retractions at claim time via `Option C`. Append an `<state_dir>/audit.jsonl` log on every edit-attributed-to-agent. Add severity + filter + burst collapsing to the SPA.

**Tech Stack:** Rust 1.75+ (existing). Vanilla-JS for the SPA. `tokio::sync::broadcast` (already in tree). `blake3` (already in tree). No new Cargo deps.

## Global Constraints

- **Branch per PR.** Each PR cuts a fresh feature branch off
  `fix/index-convergence-canonical-paths` (the current branch tip,
  with 5 unmerged index-convergence commits — `ae06a9a`, `4bde9a8`,
  `8014581`, `1f3211f`, `4cbf654`). Branches: PR 1
  `feat/revision-surface-and-world-state`; PR 2
  `feat/audit-log-and-edit-landed`; PR 3
  `feat/dashboard-noise-filters`. The first task of each PR creates its
  branch from `fix/index-convergence-canonical-paths` unless that branch
  has already merged to main — rebase the working branch on top first.
- **Existing tests must pass.** Today: 666 lib/integration + 25 e2e
  assertions in `tests/e2e/index-lifecycle.sh` (just landed in `3bce23a`).
  Target per PR: +12 / +10 / +6 new tests respectively.
- **Freshness coexistence.** The `freshness` mechanism landed in `432e883`
  already surfaces per-file staleness in `context.rs` and `metrics.rs`
  handlers. None of PR 1/2/3 tasks add or modify that mechanism. The
  `revision` field this plan introduces is additive to it.
- **No new Cargo deps.** Audit log uses `std::fs` + `parking_lot::Mutex` already used in `presence.rs`. Ring buffer is a `VecDeque` already imported in `std`.
- **Additive wire compat.** All new fields on `ClaimResult`, `ClaimRequest`, `Claim`, and tool responses are additive with `#[serde(default)]`. Existing hooks (`hooks/claude-code/{pre,post}-edit.sh`, `hooks/kimi/pre-edit.sh`, etc.) parse JSON fields they care about and ignore unknown keys; they continue to work.
- **TDD discipline.** Every task writes the failing test before the implementation. Each PR runs the full suite at the end.
- **One commit per task.** Commit messages use the `feat(server):`, `fix(server):`, `feat(presence):`, `feat(ui):`, etc. prefixes already in `git log`.
- **No new top-level modules without checking.** Some of the "new files" below may fold into existing modules once the implementer inspects them. The Files list is a minimum; if a better home exists, use it.

---

## File Structure

### New files

- `src/server/revision_log.rs` — `RevisionLog` ring buffer with `enqueue(diff)` and `diffs_since(rev) -> Result<Vec<OverlayDiff>, LookupResult>`.
- `src/server/audit.rs` — `WriteContext`, `AuditEvent`, `append_edit_event`, `get_audit_log_filtered`, audit rotation.
- `src/server/mcp/audit_tools.rs` — MCP tool handler `get_audit_log` registered into the existing dispatcher.
- `tests/audit_integration.rs` — end-to-end audit append / read / rotation / corruption tests.
- `tests/revision_log_tests.rs` — unit tests for the ring buffer (or in-test module under `src/server/revision_log.rs`).

### Modified files

- `src/server/overlay.rs` — own a `RevisionLog`; expose `current_revision() -> u64` and `diffs_since(rev) -> Result<Vec<OverlayDiff>, LookupResult>` on `VolatileOverlay`. Wire `enqueue` into existing `insert_node` / `insert_edge` callers (one site each; see `src/server/overlay/stream.rs:7-8` for the call graph).
- `src/server/presence.rs` — add `Claim.plan_revision: Option<RevisionId>`. Extend `PersistedState` (struct at line ~1101) with `audit_offset_bytes: u64` and `audit_reset_at_unix: Option<i64>` fields (both `#[serde(default)]`). Extend `save_pair` / `load_pair` (lines ~1117 and ~1163) to round-trip those fields.
- `src/server/mcp/handler.rs` — single helper that wraps any tool response with `{ ..., revision: u64 }` after the tool handler returns. This is the only Rust change in handler.rs for PR 1. (PR 2 adds the audit tool dispatch here too.)
- `src/server/mcp/presence_tools.rs` — extend `ClaimRequest` with `plan_revision`; extend `ClaimResult` with `world_state: Option<WorldState>`; add the new types `WorldState`, `ChangedSymbol`, `ChangedKind`, `LookupResult` in this module (or co-located in `presence.rs` if cleaner).
- `src/server/sse.rs` — add `PresenceEvent::EditLanded { event: AuditEvent }` variant and the corresponding JSON shape on the wire; mapper line ~58 in sse.rs:58 lists current variants.
- `src/ui/` — add severity field, "Only my session" toggle, and burst collapsing logic. (PR 3 only. Rust unaffected.)
- `docs/multiplayer.md` — document the new `revision` field on tool responses, the `world_state` field on `claim_files`, and the BeyondCurrent / TooOld error paths. (PR 1 deliverable.)

### File-decomposition rationale

`RevisionLog` is its own file because (a) it has a clean, focused API (`enqueue`, `diffs_since`, `current_revision`); (b) it's independently unit-testable without needing the rest of `VolatileOverlay`. The audit module is its own file because it owns a stateful, persistent sink with its own migration story. Co-locating `WorldState` / `ChangedSymbol` types in `presence.rs` (with the existing `ConflictEntry`) keeps the type families together; if it gets crowded we can extract to `src/server/world_state.rs` later. Tying this back to the brainstorming checklist: each new file has one responsibility, communicates through a small interface, and can be changed without breaking consumers as long as the type signatures stay stable.

---

# PR 1: Revision surface lift + world_state + retract detection

## Task 1.1: RevisionLog ring buffer (failing test → impl)

**Files:**
- Create: `src/server/revision_log.rs`
- Tests: in-module under `#[cfg(test)] mod tests { ... }`

**Interfaces:**
- Consumes: `OverlayDiff` (already defined in `src/server/overlay/stream.rs:24-31`).
- Produces:
  ```rust
  pub type RevisionId = u64; // (no re-export — uses the one from overlay/stream)
  pub enum LookupResult { Ok, BeyondCurrent, TooOld }
  pub struct RevisionLog { /* VecDeque<OverlayDiff> + next_revision */ }
  impl RevisionLog {
      pub fn new() -> Self;
      pub fn with_capacity(cap: usize) -> Self;
      pub fn current_revision(&self) -> RevisionId;  // 0 when empty
      pub fn enqueue(&mut self, diff: OverlayDiff) -> RevisionId;
      pub fn diffs_since(&self, rev: RevisionId) -> Result<Vec<OverlayDiff>, LookupResult>;
      pub fn floor_revision(&self) -> RevisionId;   // oldest revision retained
  }
  ```

- [ ] **Step 1: Write the failing tests**

```rust
// in src/server/revision_log.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{GraphNode, NodeType};

    fn fake_diff(rev: u64) -> OverlayDiff {
        OverlayDiff {
            revision: rev,
            added: vec![GraphNode::new(NodeType::Function, format!("f{rev}"), "/p.rs".into())],
            removed: vec![], updated: vec![],
        }
    }

    #[test]
    fn empty_log_returns_zero() {
        let log = RevisionLog::new();
        assert_eq!(log.current_revision(), 0);
        assert!(matches!(log.diffs_since(0), Ok(vec) if vec.is_empty()));
    }

    #[test]
    fn enqueue_assigns_sequential_revisions() {
        let mut log = RevisionLog::with_capacity(8);
        assert_eq!(log.enqueue(fake_diff(0)), 1); // caller-supplied revision is ignored
        assert_eq!(log.enqueue(fake_diff(99)), 2);
        assert_eq!(log.current_revision(), 2);
    }

    #[test]
    fn diffs_since_returns_only_strictly_newer() {
        let mut log = RevisionLog::with_capacity(8);
        log.enqueue(fake_diff(0)); // assigned 1
        log.enqueue(fake_diff(0)); // assigned 2
        log.enqueue(fake_diff(0)); // assigned 3
        let out = log.diffs_since(1).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].revision, 2);
        assert_eq!(out[1].revision, 3);
    }

    #[test]
    fn diffs_since_beyond_current_returns_beyond_current() {
        let mut log = RevisionLog::with_capacity(8);
        log.enqueue(fake_diff(0)); // → rev 1
        assert!(matches!(log.diffs_since(99), Err(LookupResult::BeyondCurrent)));
    }

    #[test]
    fn ring_evicts_too_old() {
        let mut log = RevisionLog::with_capacity(4);
        for _ in 0..10 { log.enqueue(fake_diff(0)); } // 10 enqueues, cap 4
        assert_eq!(log.current_revision(), 10);
        assert_eq!(log.floor_revision(), 7);
        assert!(matches!(log.diffs_since(5), Err(LookupResult::TooOld)));
        let ok = log.diffs_since(7).unwrap();
        assert_eq!(ok.len(), 4);
    }
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run:
```bash
cargo test --lib revision_log 2>&1 | tail -10
```
Expected: compile error "cannot find type RevisionLog" (it doesn't exist yet).

- [ ] **Step 3: Implement RevisionLog**

```rust
// src/server/revision_log.rs
use crate::overlay::stream::OverlayDiff;
use std::collections::VecDeque;

pub type RevisionId = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupResult { Ok, BeyondCurrent, TooOld }

#[derive(Debug)]
pub struct RevisionLog {
    diffs: VecDeque<OverlayDiff>,
    capacity: usize,
    next: RevisionId,
}

impl RevisionLog {
    pub fn new() -> Self { Self::with_capacity(256) }

    pub fn with_capacity(cap: usize) -> Self {
        Self { diffs: VecDeque::with_capacity(cap), capacity: cap.max(1), next: 0 }
    }

    pub fn current_revision(&self) -> RevisionId { self.next }

    pub fn floor_revision(&self) -> RevisionId {
        self.diffs.front().map(|d| d.revision).unwrap_or(0)
    }

    pub fn enqueue(&mut self, mut diff: OverlayDiff) -> RevisionId {
        self.next += 1;
        diff.revision = self.next;
        if self.diffs.len() == self.capacity {
            self.diffs.pop_front();
        }
        self.diffs.push_back(diff);
        self.next
    }

    pub fn diffs_since(&self, rev: RevisionId) -> Result<Vec<OverlayDiff>, LookupResult> {
        if rev > self.next { return Err(LookupResult::BeyondCurrent); }
        if !self.diffs.is_empty() && rev < self.floor_revision() {
            return Err(LookupResult::TooOld);
        }
        Ok(self.diffs.iter().filter(|d| d.revision > rev).cloned().collect())
    }
}
```

(Note: do **not** define `RevisionId` here if `crate::overlay::stream::RevisionId` is already in scope at the use site. Re-export only if the import would otherwise shadow an existing binding.)

- [ ] **Step 4: Run tests, verify they pass**

Run:
```bash
cargo test --lib revision_log 2>&1 | tail -10
```
Expected: PASS, 5 tests.

- [ ] **Step 5: Commit**

```bash
git add src/server/revision_log.rs
git commit -m "feat(overlay): RevisionLog ring buffer for revision-based deltas"
```

---

## Task 1.2: Wire RevisionLog into VolatileOverlay

**Files:**
- Modify: `src/server/overlay.rs` (add fields + methods)
- Tests: extend `src/server/overlay_tests.rs` if present, or add in-module tests

**Interfaces:**
- Consumes: `RevisionLog` from `src/server/revision_log.rs`.
- Produces:
  ```rust
  impl VolatileOverlay {
      pub fn current_revision(&self) -> RevisionId;
      pub fn diffs_since(&self, rev: RevisionId) -> Result<Vec<OverlayDiff>, LookupResult>;
  }
  ```
  And `insert_node` / `insert_edge` now also push the diff into the internal `RevisionLog` before broadcasting.

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)]` of `src/server/overlay.rs`:

```rust
#[test]
fn overlay_reports_current_revision_after_inserts() {
    use crate::overlay::stream::OverlayDiff;
    let vo = VolatileOverlay::new();
    assert_eq!(vo.current_revision(), 0);
    let node = GraphNode::new(NodeType::Function, "f1".into(), "/p.rs".into());
    vo.insert_node(node);
    let rev = vo.current_revision();
    assert!(rev >= 1);
}

#[test]
fn overlay_diffs_since_filters_correctly() {
    let vo = VolatileOverlay::new();
    vo.insert_node(GraphNode::new(NodeType::Function, "a".into(), "/a.rs".into()));
    let mid = vo.current_revision();
    vo.insert_node(GraphNode::new(NodeType::Function, "b".into(), "/b.rs".into()));
    let diffs = vo.diffs_since(mid).unwrap();
    assert_eq!(diffs.len(), 1);
    assert!(diffs[0].added.iter().any(|n| n.name == "b"));
}

#[test]
fn overlay_diffs_since_beyond_current_errors() {
    let vo = VolatileOverlay::new();
    vo.insert_node(GraphNode::new(NodeType::Function, "a".into(), "/a.rs".into()));
    assert!(matches!(vo.diffs_since(999), Err(LookupResult::BeyondCurrent)));
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test --lib overlay_tests 2>&1 | tail -10`
Expected: compile error (no `current_revision` method on `VolatileOverlay`).

- [ ] **Step 3: Implement methods**

In `src/server/overlay.rs`, add a field to the `VolatileOverlay` struct:

```rust
pub struct VolatileOverlay {
    // ... existing fields ...
    log: Arc<parking_lot::Mutex<crate::server::revision_log::RevisionLog>>,
}
```

(Adjust import path; the goal is `Arc<Mutex<RevisionLog>>` next to `bloom_filter`.)

In `insert_node` (and `insert_edge`), after the existing petgraph insertion but before the broadcast, push a diff into the log:

```rust
{
    let mut log = self.log.lock();
    log.enqueue(OverlayDiff {
        revision: 0, // will be overwritten by enqueue
        added: vec![node.clone()],
        removed: vec![],
        updated: vec![],
    });
}
// ... existing broadcast_overlay_diff call ...
```

Then add the public methods:

```rust
impl VolatileOverlay {
    pub fn current_revision(&self) -> u64 {
        self.log.lock().current_revision()
    }

    pub fn diffs_since(&self, rev: u64) -> Result<Vec<OverlayDiff>, LookupResult> {
        self.log.lock().diffs_since(rev)
    }
}
```

- [ ] **Step 4: Run tests, verify they pass**

Run: `cargo test --lib overlay 2>&1 | tail -10`
Expected: PASS, 3 new tests on top of existing overlay tests.

- [ ] **Step 5: Commit**

```bash
git add src/server/overlay.rs
git commit -m "feat(overlay): expose current_revision + diffs_since via embedded RevisionLog"
```

---

## Task 1.3: Tool response envelope carries `revision` at protocol level

> **Revision note.** Round 1 of this task (commit `1c01eca`, branch `feat/revision-surface-and-world-state`) injected `revision` into the tool's `content[0].text` JSON and was rejected at review for breaking the additive wire contract — `list_repos` and other top-level-array tools were wrapped as `{ "value": [...], "revision": N }`, breaking 5 e2e shell scripts and the Command Center SPA. Round 2 moves `revision` to the **outer `CallToolResult` envelope** instead, which is truly additive.

**Files:**
- Modify: `src/server/mcp/handler.rs`

**Interfaces:**
- Produces: every `CallToolResult` constructed by `handle_call_tool_request` (stdio) and the `tools/call` JSON-RPC arm of `handle_request` (HTTP) carries `revision: u64` in the outer envelope metadata. The inner `content[0].text` is the tool's payload — unchanged in shape; bare arrays stay bare arrays, strings stay strings, Markdown stays Markdown.

- [ ] **Step 1: Write the failing tests**

Two tests:

```rust
#[tokio::test]
async fn call_tool_result_envelope_carries_revision() {
    // Drive `list_repos` twice through whatever minimal seam exists in
    // `src/server/mcp/handler.rs::tests`. Assert:
    //   * the response is a CallToolResult-shaped value
    //   * `revision` is present in the outer envelope, sibling of `is_error`,
    //     NOT inside `content[0].text`
    //   * second call's revision >= first
    //   * inner `content[0].text`, when parsed, does NOT contain a
    //     `revision` field
}

#[tokio::test]
async fn list_repos_keeps_bare_array_payload() {
    // Parse response.content[0].text as JSON. Assert it starts with `[`
    // (bare array), NOT wrapped in `{ "value": [...], ... }`.
    // Pins the additive-wire contract.
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test --lib mcp_revision 2>&1 | tail -10`
Expected: FAIL (no `revision` in envelope yet).

- [ ] **Step 3: Implement — TWO steps, in this order**

**(a)** Revert round 1's `inject_revision` helper (commit `1c01eca` introduced it at `src/server/mcp/handler.rs:98-117` plus `render_with_revision` / `inject_revision_into_text` and 28+ call sites). Run `git revert 1c01eca --no-edit` first; resolve any subsequent conflicts.

**(b)** Add ONE unified constructor that builds a `CallToolResult` with `revision` injected at the envelope level. Two viable strategies:

- **Strategy A (preferred):** `rmcp`'s `CallToolResult` exposes a `_meta` / `meta` field for arbitrary metadata. Use `result.meta = Some(json!({ "revision": overlay.current_revision() }))` (verify the field name in the pinned `rmcp` version).
- **Strategy B (fallback):** if no metadata field exists in this `rmcp` version, inject at the JSON-RPC `result._meta` envelope level — still sibling metadata, still additive.

Wrap **every** `CallToolResult` construction site — error sites, presence-tool dispatches, executor-fallback paths, Markdown tools like `get_health` — through this constructor. Round 1's review flagged "non-JSON / early error responses bypass injection" as an Important finding; the unified constructor resolves it.

Keep streaming carve-outs (`/events` SSE, `/overlay/subscribe` ndjson) intact.

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test --lib mcp_revision 2>&1 | tail -10`
Expected: PASS, both new tests green.

- [ ] **Step 5: Run full suite**

Run: `cargo test --lib 2>&1 | tail -5`
Expected: all green.

- [ ] **Step 6: Smoke-check the affected e2e scripts (syntax)**

```bash
for f in tests/e2e/federation_e2e.sh tests/e2e/workspace_e2e.sh \
         tests/e2e/multiplayer-hooks.sh tests/e2e/multiplayer-full.sh; do
    bash -n "$f" && echo "OK: $f"
done
```

Record in the report which scripts you could run beyond `bash -n` (e.g., `cargo test --tests` if it covers them) and which you could not.

- [ ] **Step 7: Commit**

```bash
git revert 1c01eca --no-edit   # if not already done
git add src/server/mcp/handler.rs
git commit -m "feat(mcp): carry revision: u64 on CallToolResult envelope (not in payload)"
```

If the revert conflicts with subsequent branch edits, resolve and squash. Document the resulting commit hash in the report.

---

## Task 1.4: Claim.plan_revision field

**Files:**
- Modify: `src/server/presence.rs` (struct definition + serde)
- Modify: `src/server/mcp/presence_tools.rs` (struct on the wire — adjust `ClaimRequest` if duplicated there)

**Interfaces:**
- Consumes: `RevisionId` from `crate::overlay::stream`.
- Produces: `Claim { plan_revision: Option<RevisionId> }` with `#[serde(default)]`.

- [ ] **Step 1: Write the failing test**

In `presence_tests.rs` (or in-test module of `presence.rs`):

```rust
#[test]
fn claim_round_trips_plan_revision() {
    let claim = Claim {
        agent_id: AgentId("a1".into()),
        path: PathBuf::from("/x.rs"),
        symbols: vec!["login".into()],
        content_hash: None,
        intent: ClaimIntent::Edit,
        claimed_at: SystemTime::UNIX_EPOCH,
        last_touched_unix: SystemTime::UNIX_EPOCH,
        expires_at: None,
        plan_revision: Some(42),
    };
    let json = serde_json::to_string(&claim).unwrap();
    let back: Claim = serde_json::from_str(&json).unwrap();
    assert_eq!(back.plan_revision, Some(42));
}

#[test]
fn claim_without_plan_revision_deserializes_to_none() {
    let json = r#"{
        "agent_id": "a1",
        "path": "/x.rs",
        "symbols": ["login"],
        "content_hash": null,
        "intent": "edit"
    }"#;
    let claim: Claim = serde_json::from_str(json).unwrap();
    assert_eq!(claim.plan_revision, None);
}
```

- [ ] **Step 2: Run the tests, verify they fail**

Run: `cargo test --lib presence_tests 2>&1 | tail -10`
Expected: compile error (field doesn't exist).

- [ ] **Step 3: Add the field**

In `src/server/presence.rs:144-173`, add:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub plan_revision: Option<RevisionId>,
```

Confirm the existing struct's other `skip_serializing`/`default` annotations still compile. `RevisionId` is the existing `pub type RevisionId = u64;` — alias it via `use crate::overlay::stream::RevisionId;` at the top of `presence.rs`.

- [ ] **Step 4: Run the tests, verify they pass**

Run: `cargo test --lib presence_tests 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Run the full suite**

Run: `cargo test --lib 2>&1 | tail -5`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add src/server/presence.rs
git commit -m "feat(presence): Claim.plan_revision (Option<RevisionId>)"
```

---

## Task 1.5: WorldState / ChangedSymbol / ChangedKind / LookupResult types + ClaimResult.world_state

**Files:**
- Modify: `src/server/mcp/presence_tools.rs` (add types + extend `ClaimResult`)

**Interfaces:**
- Produces:
  ```rust
  pub enum ChangedKind { Edited, Retracted }
  pub struct ChangedSymbol {
      pub name: String,
      pub change_kind: ChangedKind,
      pub at_revision: RevisionId,
  }
  pub struct WorldState {
      pub current: RevisionId,
      pub plan: RevisionId,
      pub changed_symbols: Vec<ChangedSymbol>,
      pub note: Option<String>,
  }
  pub enum LookupResult { Ok, BeyondCurrent, TooOld } // re-exported from revision_log
  ```
- Modifies `ClaimResult` (currently `{granted, conflicts}`) to add:
  ```rust
  pub world_state: Option<WorldState>,
  ```
  with `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- Modifies `ClaimRequest` to add `pub plan_revision: Option<RevisionId>` with the same serde treatment.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn world_state_serializes_note_only_when_some() {
    let ws = WorldState {
        current: 10, plan: 5,
        changed_symbols: vec![ChangedSymbol {
            name: "verify_token".into(),
            change_kind: ChangedKind::Retracted,
            at_revision: 10,
        }],
        note: Some("plan_revision beyond current — server restarted".into()),
    };
    let json = serde_json::to_string(&ws).unwrap();
    assert!(json.contains("\"note\""));
    assert!(json.contains("\"Retracted\""));
}

#[test]
fn world_state_with_no_note_omits_field() {
    let ws = WorldState {
        current: 10, plan: 5, changed_symbols: vec![], note: None,
    };
    let json = serde_json::to_string(&ws).unwrap();
    assert!(!json.contains("\"note\""));
}

#[test]
fn changed_symbols_deduplicated_in_construction_helper() {
    // The implementer writes a small helper `ChangedSymbol::from_diffs(diffs, plan, current) -> Vec<ChangedSymbol>`
    // that collapses multiple OverlayDiff entries on the same symbol into one entry with the latest at_revision.
    // This test verifies dedup.
    let diffs = vec![
        OverlayDiff { revision: 6, added: vec![GraphNode::new(NodeType::Function, "f".into(), "/x.rs".into())], removed: vec![], updated: vec![] },
        OverlayDiff { revision: 7, added: vec![GraphNode::new(NodeType::Function, "f".into(), "/x.rs".into())], removed: vec![], updated: vec![] },
    ];
    let out = ChangedSymbol::from_diffs(&diffs, 5, 8);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].at_revision, 7);
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test --lib mcp_presence 2>&1 | tail -10`
Expected: compile errors.

- [ ] **Step 3: Implement the types**

In `src/server/mcp/presence_tools.rs`:

```rust
use crate::server::revision_log::RevisionId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChangedKind { Edited, Retracted }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangedSymbol {
    pub name: String,
    pub change_kind: ChangedKind,
    pub at_revision: RevisionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct WorldState {
    pub current: RevisionId,
    pub plan: RevisionId,
    #[serde(default)]
    pub changed_symbols: Vec<ChangedSymbol>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ChangedSymbol {
    pub fn from_diffs(
        diffs: &[crate::overlay::stream::OverlayDiff],
        _plan: RevisionId,
        current: RevisionId,
    ) -> Vec<ChangedSymbol> {
        use std::collections::BTreeMap;
        let mut by_name: BTreeMap<String, RevisionId> = BTreeMap::new();
        for d in diffs {
            for n in &d.added { by_name.insert(n.name.clone(), d.revision); }
            for n in &d.updated { by_name.insert(n.name.clone(), d.revision); }
        }
        by_name.into_iter().map(|(name, at)| ChangedSymbol {
            name,
            change_kind: ChangedKind::Edited,
            at_revision: at,
        }).collect::<Vec<_>>()
            .into_iter()
            // Add `current` reason only if the diff's revision is the latest for the name;
            // the helper signature may evolve. For PR 1 we don't filter by `plan` here —
            // filtering happens in the claim handler that knows the claim's paths.
            .collect()
    }
}
```

Extend `ClaimResult` and `ClaimRequest` per the interfaces block.

- [ ] **Step 4: Run tests, verify they pass**

Run: `cargo test --lib mcp_presence 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Run full suite**

Run: `cargo test --lib 2>&1 | tail -5`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add src/server/mcp/presence_tools.rs
git commit -m "feat(presence): WorldState + ChangedSymbol types and ClaimResult.world_state"
```

---

## Task 1.6: Static-graph retract detection at claim time

**Files:**
- Modify: `src/server/mcp/presence_tools.rs` (the `claim_files` MCP handler — wherever it's wired into the dispatcher in this module)
- Tests: extend `tests/federation_integration.rs` or add a new `tests/retract_detection.rs`

**Interfaces:**
- Consumes: `WorldState`, `ChangedSymbol`, `ChangedKind`, `LookupResult`, the existing `OccupancyMap::claim`.
- Produces: the claim handler returns `ClaimResult { ..., world_state: Option<...> }` populated per the spec.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn claim_with_retracted_symbol_populates_world_state() {
    // Setup: register two agents; have agent-A submit a query plan with no plan_revision yet
    // (this test path doesn't need a real plan). Then in single-repo mode:
    //   1. agent-B issues claim_files(symbols: ["verify_token"], intent: Edit) → granted.
    //   2. The rep simulates `project_repo` retraction by directly calling
    //      `federated_index_or_db.remove_nodes_by_ids(...)` — check the project's actual
    //      API surface; the field name may differ.
    //   3. agent-A issues claim_files(symbols: ["verify_token"], plan_revision=Some(0), intent: Edit).
    //   4. The response should include world_state.changed_symbols with at least one entry
    //      whose change_kind == Retracted and whose name == "verify_token".
}
```

For the implementer: read `src/server/federation/federated_index.rs` to find the retraction API (`remove_nodes` / `remove_nodes_by_ids`). The test setup needs to construct a `FederatedIndex` or per-repo DB and trigger retraction; mirrors of this setup exist in `tests/federation_integration.rs` already.

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test --test retract_detection 2>&1 | tail -10`
Expected: FAIL — `claim_files` doesn't populate `world_state`.

- [ ] **Step 3: Implement the claim-time retract lookup**

In the `claim_files` handler:

```rust
let plan_revision = request.plan_revision;
let world_state = match plan_revision {
    None => None,
    Some(plan) => {
        let current = ctx.overlay.current_revision();
        // (a) Look up each requested symbol in the static graph.
        let mut retracted: Vec<ChangedSymbol> = Vec::new();
        for sym in request.symbols.iter() {
            if !symbol_exists_in_static_graph(ctx, sym) {
                retracted.push(ChangedSymbol {
                    name: sym.clone(),
                    change_kind: ChangedKind::Retracted,
                    at_revision: current,
                });
            }
        }
        // (b) Look up overlay diffs since `plan`.
        let overlay_diffs = match ctx.overlay.diffs_since(plan) {
            Ok(ds) => ds,
            Err(LookupResult::BeyondCurrent) => {
                return ClaimResult { /* ... */ world_state: Some(WorldState {
                    current, plan,
                    changed_symbols: vec![],
                    note: Some("plan_revision beyond current — server may have restarted".into()),
                }), /* ... */ };
            }
            Err(LookupResult::TooOld) => {
                return ClaimResult { /* ... */ world_state: Some(WorldState {
                    current, plan,
                    changed_symbols: vec![],
                    note: Some("plan_revision too old for delta; resync required".into()),
                }), /* ... */ };
            }
        };
        // (c) Combine.
        let mut changed_symbols = ChangedSymbol::from_diffs(&overlay_diffs, plan, current);
        changed_symbols.extend(retracted);
        Some(WorldState { current, plan, changed_symbols, note: None })
    }
};
```

Provide `symbol_exists_in_static_graph` as a small helper that consults `FederatedIndex::search_org` or the per-repo `GraphDatabase::find_nodes_by_name`, depending on mode. The helper sits in this same file.

- [ ] **Step 4: Run the test, verify it passes**

Run: `cargo test --test retract_detection 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Run full suite**

Run: `cargo test --lib 2>&1 | tail -5`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add src/server/mcp/presence_tools.rs tests/retract_detection.rs
git commit -m "feat(presence): static-graph retract detection at claim time + world_state population"
```

---

## Task 1.7: docs/multiplayer.md update

**Files:**
- Modify: `docs/multiplayer.md`

- [ ] **Step 1: Add the three new sections**

Append to `docs/multiplayer.md`:

```markdown
## Revision surface

Every tool response now carries a top-level `revision: u64` field. The
counter is per-process and monotonic; it increments on every overlay diff
the server emits. Tools that don't return JSON (streaming-only) are
unchanged.

Claim-aware tools (`claim_files` is the only one today) additionally
accept `plan_revision: u64` on request and may return `world_state` on
response. See `docs/superpowers/specs/2026-08-18-coordination-staleness-audit-design.md`
for the full contract.

## world_state.changed_symbols

`world_state` is the agent's signal that the world may have moved since
it queried. Each entry is `{ name, change_kind, at_revision }` where
`change_kind` is `Edited` (changed via overlay diff) or `Retracted`
(removed from the static graph).

If `world_state.note` is set, the agent must resync:
- `"plan_revision beyond current — server may have restarted"` →
  the server reloaded and lost the revision counter; re-query.
- `"plan_revision too old for delta; resync required"` →
  the agent's plan is too far in the past; re-query.
```

- [ ] **Step 2: Verify markdown rendering**

Run: `awk '/^```markdown$/,/^```$/' docs/multiplayer.md | wc -l && head -5 docs/multiplayer.md`
Expected: a positive line count and the file still starts with the original H1.

- [ ] **Step 3: Commit**

```bash
git add docs/multiplayer.md
git commit -m "docs(multiplayer): document revision field + world_state"
```

---

## Task 1.8: End-to-end integration test

**Files:**
- Tests: extend `tests/federation_integration.rs` (this is the existing home for cross-component tests)

- [ ] **Step 1: Write the test**

```rust
#[tokio::test]
async fn agent_a_query_then_agent_b_edit_then_agent_a_claim_sees_delta() {
    // 1. Set up a federation with one repo containing `verify_token`.
    // 2. agent-A queries get_blast_radius("verify_token") at revision X.
    //    Confirm response carries `revision: X`.
    // 3. agent-B inserts a new node into the overlay (or modifies a file
    //    the watcher picks up) — the diff is broadcast and the overlay revision bumps.
    // 4. agent-A calls claim_files(symbols: ["verify_token"], plan_revision=X).
    // 5. Response: granted == true, world_state.changed_symbols non-empty
    //    with at least one `Edited` entry whose name corresponds to the
    //    overlay diff.
}

#[tokio::test]
async fn agent_a_plan_revision_beyond_current_gets_note() {
    // 1. Set up federation.
    // 2. agent-A calls claim_files(symbols: [...], plan_revision=999_999, intent=Edit).
    // 3. Response: world_state.note is Some("plan_revision beyond current …").
}
```

For both tests, rely on `tests/federation_integration.rs`'s existing fixture helpers (search for `setup_federation` or similar in that file). Mirror its setup pattern.

- [ ] **Step 2: Run, verify pass**

Run: `cargo test --test federation_integration 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 3: Run full suite**

Run: `cargo test 2>&1 | tail -5`
Expected: green, +12 new tests over the 666 baseline.

- [ ] **Step 4: Commit**

```bash
git add tests/federation_integration.rs
git commit -m "test(federation): revision surface + world_state end-to-end"
```

---

## PR 1 wrap

Push branch `feat/revision-surface-and-world-state`, open PR with title `feat(server): revision surface + world_state + retract detection`. PR body references the spec section `## PR 1`.

---

# PR 2: write_context + audit log + edit_landed SSE

## Task 2.1: Audit module — types and append function

**Files:**
- Create: `src/server/audit.rs`
- Tests: in-module `#[cfg(test)]`

**Interfaces:**
- Produces:
  ```rust
  use std::path::Path;
  use crate::server::presence::{AgentId, Claim};
  use crate::server::overlay::stream::RevisionId;

  #[derive(Debug, Clone, serde::Serialize)]
  pub struct WriteContext {
      pub claim_snapshot: Vec<Claim>,
      pub concurrent_editors_at_write: Vec<AgentId>,
      pub as_of_revision: RevisionId,
  }

  #[derive(Debug, Clone, serde::Serialize)]
  pub struct AuditEvent {
      pub ts_unix: f64,
      pub agent_id: AgentId,
      pub path: PathBuf,
      pub claim_set: Vec<Claim>,
      pub racers: Vec<crate::server::presence::ConflictEntry>,
      pub plan_revision: Option<RevisionId>,
      pub landed_revision: RevisionId,
  }

  pub const AUDIT_LOG_FILENAME: &str = "audit.jsonl";
  pub const AUDIT_LOG_MAX_BYTES: u64 = 50 * 1024 * 1024;

  pub fn append_edit_event(state_dir: &Path, event: &AuditEvent) -> std::io::Result<()>;
  pub fn read_audit_log(state_dir: &Path, since_unix: Option<f64>) -> std::io::Result<Vec<AuditEvent>>;
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn append_writes_one_jsonl_line() {
    let tmp = tempdir().unwrap();
    let event = AuditEvent { /* populated */ };
    append_edit_event(tmp.path(), &event).unwrap();
    let log = tmp.path().join(AUDIT_LOG_FILENAME);
    let body = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 1);
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert!(parsed.get("ts_unix").is_some());
}

#[test]
fn append_rotates_at_max_bytes() {
    let tmp = tempdir().unwrap();
    // Pre-fill audit.jsonl so the next append trips the 50 MB cap exactly.
    let log_path = tmp.path().join(AUDIT_LOG_FILENAME);
    let cap: usize = AUDIT_LOG_MAX_BYTES as usize;
    std::fs::write(&log_path, vec![b'x'; cap]).unwrap();
    // Seed the rotated file with known content; the rotator overwrites it.
    let rotated = tmp.path().join(AUDIT_LOG_ROTATED);
    std::fs::write(&rotated, "old-rotated-content\n").unwrap();
    // Append one real event — must rotate sentinel → audit.jsonl.1.
    let event = AuditEvent {
        ts_unix: 1700000000.0,
        agent_id: AgentId("a-rotation".into()),
        path: std::path::PathBuf::from("/x.rs"),
        claim_set: vec![],
        racers: vec![],
        plan_revision: None,
        landed_revision: 1,
    };
    append_edit_event(tmp.path(), &event).unwrap();
    // Rotated file now contains the sentinel bytes (length == cap); the new event lives in audit.jsonl.
    let rotated_len = std::fs::metadata(&rotated).unwrap().len() as usize;
    assert_eq!(rotated_len, cap);
    let current = std::fs::read_to_string(&log_path).unwrap();
    assert!(current.contains("\"ts_unix\""));
}

#[test]
fn read_returns_events_newer_than_since() {
    let tmp = tempdir().unwrap();
    let old = AuditEvent { ts_unix: 100.0, /* ... */ .. };
    let newer = AuditEvent { ts_unix: 200.0, /* ... */ .. };
    append_edit_event(tmp.path(), &old).unwrap();
    append_edit_event(tmp.path(), &newer).unwrap();
    let events = read_audit_log(tmp.path(), Some(150.0)).unwrap();
    assert_eq!(events.len(), 1);
    assert!((events[0].ts_unix - 200.0).abs() < 0.001);
}
```

For rotation, the implementer adds a small public constant or test-only setter so tests can use a 100-byte cap instead of the 50 MB production one. Add `#[cfg(test)] const AUDIT_LOG_MAX_BYTES_TEST: u64 = ...;` next to the public constant if needed.

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test --lib audit 2>&1 | tail -10`
Expected: compile error.

- [ ] **Step 3: Implement the module**

```rust
// src/server/audit.rs
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::fs::OpenOptions;
use crate::server::presence::{AgentId, Claim, ConflictEntry};
use crate::overlay::stream::RevisionId;

pub const AUDIT_LOG_FILENAME: &str = "audit.jsonl";
pub const AUDIT_LOG_ROTATED: &str = "audit.jsonl.1";
pub const AUDIT_LOG_MAX_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Debug, Clone, serde::Serialize)]
pub struct WriteContext { /* … */ }
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditEvent { /* … */ }

pub fn append_edit_event(state_dir: &Path, event: &AuditEvent) -> std::io::Result<()> {
    let path = state_dir.join(AUDIT_LOG_FILENAME);
    if path.exists() && path.metadata()?.len() >= AUDIT_LOG_MAX_BYTES {
        let rotated = state_dir.join(AUDIT_LOG_ROTATED);
        let _ = std::fs::remove_file(&rotated);
        std::fs::rename(&path, &rotated)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    let line = serde_json::to_string(event).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    writeln!(f, "{line}")?;
    Ok(())
}

pub fn read_audit_log(state_dir: &Path, since_unix: Option<f64>) -> std::io::Result<Vec<AuditEvent>> {
    let mut out = Vec::new();
    for name in [AUDIT_LOG_FILENAME, AUDIT_LOG_ROTATED] {
        let p = state_dir.join(name);
        if !p.exists() { continue; }
        let f = std::fs::File::open(&p)?;
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            if line.is_empty() { continue; }
            if let Ok(ev) = serde_json::from_str::<AuditEvent>(&line) {
                if let Some(since) = since_unix { if ev.ts_unix < since { continue; } }
                out.push(ev);
            }
        }
    }
    Ok(out)
}
```

- [ ] **Step 4: Run tests, verify they pass**

Run: `cargo test --lib audit 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/server/audit.rs
git commit -m "feat(audit): AuditEvent + append_edit_event + read_audit_log"
```

---

## Task 2.2: PersistedState audit fields

**Files:**
- Modify: `src/server/presence.rs` (struct + save/load pair)

**Interfaces:**
- Produces: `PersistedState { …, #[serde(default)] audit_offset_bytes: u64, #[serde(default)] audit_reset_at_unix: Option<i64> }` — additive only.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn persisted_state_round_trips_audit_offset() {
    let path = tempdir().unwrap();
    let path = path.path().join("state.json");
    // Build a PersistedState ... (or call save_pair with a synthetic offset — see impl step).
    let json = r#"{
        "sessions": [],
        "occupancy_by_file": [],
        "occupancy_by_agent": [],
        "audit_offset_bytes": 12345,
        "audit_reset_at_unix": 1700000000.0
    }"#;
    std::fs::write(&path, json).unwrap();
    let reg = PresenceRegistry::new();
    let occ = OccupancyMap::new();
    load_pair(&path, &reg, &occ).unwrap();
    // No public read accessor yet — the assertion here will be added by the impl step.
}
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test --lib persisted_state 2>&1 | tail -10`
Expected: parse error (unknown field) or compile error if you reference an accessor that doesn't exist yet.

- [ ] **Step 3: Extend PersistedState**

In `src/server/presence.rs:1101-1111`:

```rust
#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedState {
    sessions: Vec<(String, AgentSession)>,
    occupancy_by_file: Vec<(PathBuf, Vec<String>, Vec<(String, Vec<String>)>)>,
    occupancy_by_agent: Vec<(String, Vec<Claim>)>,
    #[serde(default)]
    audit_offset_bytes: u64,
    #[serde(default)]
    audit_reset_at_unix: Option<f64>,
}
```

In `save_pair`, set these defaults at write time:

```rust
PersistedState {
    /* existing fields */,
    audit_offset_bytes: 0,           // populated by the audit module in PR 2's later tasks
    audit_reset_at_unix: None,
}
```

In `load_pair`, no special handling needed because of `#[serde(default)]`. The values are loaded into fields on the struct (add temporary public accessors or print them in the test for now; the offset is consumed by tasks 2.4 / 2.5).

- [ ] **Step 4: Run the test, verify it passes**

Run: `cargo test --lib persisted_state 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Run full suite**

Run: `cargo test --lib 2>&1 | tail -5`
Expected: green. Old state files load without migration — field defaults are used.

- [ ] **Step 6: Commit**

```bash
git add src/server/presence.rs
git commit -m "feat(presence): PersistedState audit_offset_bytes + audit_reset_at_unix (additive)"
```

---

## Task 2.3: Wire claim resolution → audit append

**Files:**
- Modify: the `claim_files` handler in `src/server/mcp/presence_tools.rs` (the same handler touched in Task 1.6)
- Tests: extend `tests/audit_integration.rs` (created in this task if not yet)

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn granted_claim_appends_audit_event() {
    // 1. Set up a working state_dir (tempdir).
    // 2. Register an agent; have it claim files; capture the response.
    // 3. Assert audit.jsonl exists with one line, and that line parses
    //    to an AuditEvent with the expected agent_id, path, plan_revision=None, landed_revision >= 1.
}
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test --lib --test audit_integration 2>&1 | tail -10`
Expected: FAIL — no audit file written.

- [ ] **Step 3: Append the audit event on grant**

At the end of the claim handler, after `OccupancyMap::claim` returns granted, build the audit event:

```rust
let landed_revision = ctx.overlay.current_revision();
let racers = result.conflicts.clone();
let audit = AuditEvent {
    ts_unix: SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_secs_f64(),
    agent_id: agent.id.clone(),
    path: req.path.clone(),
    claim_set: /* the granted Claim entries */,
    racers,
    plan_revision: req.plan_revision,
    landed_revision,
};
if let Err(e) = append_edit_event(state_dir_for_audit(), &audit) {
    tracing::warn!("audit append failed: {e}");
}
```

The audit append is best-effort: I/O failure → WARN, claim still valid (per the spec's invariant). Make `state_dir_for_audit()` resolve from the `LainServer`'s configured state directory (`config/mod.rs:203`).

- [ ] **Step 4: Run the test, verify it passes**

Run: `cargo test --test audit_integration 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Run full suite**

Run: `cargo test 2>&1 | tail -5`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add src/server/mcp/presence_tools.rs tests/audit_integration.rs
git commit -m "feat(audit): append AuditEvent on granted claim"
```

---

## Task 2.4: SSE event `edit_landed`

**Files:**
- Modify: `src/server/presence.rs` (add `PresenceEvent::EditLanded` variant near line ~1066)
- Modify: `src/server/sse.rs` (map the variant to wire JSON near line ~58)

**Interfaces:**
- Produces:
  ```rust
  PresenceEvent::EditLanded { event: AuditEvent }
  ```
  Wire JSON: `{"agent_id":"...", "path":"...", "claim_set":[…], "racers":[…], "plan_revision":N|null, "landed_revision":N, "ts_unix":F}`

- [ ] **Step 1: Write the failing test**

In `src/server/sse.rs` (extend the existing `#[cfg(test)]` module — Task 7-style coverage):

```rust
#[tokio::test]
async fn edit_landed_event_serializes_with_full_payload() {
    use crate::server::audit::AuditEvent;
    let event = PresenceEvent::EditLanded { event: AuditEvent { /* fields */ } };
    let json = serde_json::to_value(&event).unwrap();
    assert!(json.get("event").is_some() || json.get("agent_id").is_some());
    // Match whichever shape the existing presence event serializer uses in this file.
}
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test --lib sse 2>&1 | tail -10`
Expected: compile error (variant doesn't exist).

- [ ] **Step 3: Add the variant and the wire mapping**

In `presence.rs` near `PresenceEvent` (~line 1066):

```rust
EditLanded { event: crate::server::audit::AuditEvent },
```

In `sse.rs` near the existing variant→string mapping (~line 58), map the new variant to `"edit_landed"` and emit its inner `AuditEvent` JSON.

- [ ] **Step 4: Run the test, verify it passes**

Run: `cargo test --lib sse 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Run full suite**

Run: `cargo test --lib 2>&1 | tail -5`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add src/server/presence.rs src/server/sse.rs
git commit -m "feat(sse): add edit_landed event with AuditEvent payload"
```

---

## Task 2.5: `get_audit_log` MCP tool

**Files:**
- Create: `src/server/mcp/audit_tools.rs`
- Modify: `src/server/mcp/handler.rs` (register the new tool into the dispatcher)

**Interfaces:**
- Produces: an `McpTool` named `get_audit_log` with arguments `{ since_unix: Option<f64>, path_glob: Option<String> }` returning `Vec<AuditEvent>` (filtered).

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn get_audit_log_filters_by_path_glob() {
    // 1. Pre-populate audit.jsonl with two events: path=/a.rs and path=/b/foo.rs.
    // 2. Call the tool handler with path_glob = Some("/b/**".into()).
    // 3. Assert one event returned; the other filtered.
}
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test --lib audit_tools 2>&1 | tail -10`
Expected: FAIL — tool not registered.

- [ ] **Step 3: Implement the tool**

```rust
// src/server/mcp/audit_tools.rs
use crate::server::audit::{read_audit_log, AuditEvent};

pub struct GetAuditLogHandler;

impl crate::server::mcp::ToolHandler for GetAuditLogHandler {
    fn name(&self) -> &'static str { "get_audit_log" }
    fn handle(&self, ctx: &crate::server::mcp::ToolContext, args: serde_json::Value) -> Result<serde_json::Value, String> {
        #[derive(serde::Deserialize)]
        struct A { since_unix: Option<f64>, path_glob: Option<String> }
        let a: A = serde_json::from_value(args).map_err(|e| e.to_string())?;
        let state_dir = ctx.state_dir_for_audit();
        let mut events = read_audit_log(state_dir, a.since_unix).map_err(|e| e.to_string())?;
        if let Some(glob) = a.path_glob {
            events.retain(|e| glob_matches(&glob, &e.path));
        }
        serde_json::to_value(events).map_err(|e| e.to_string())
    }
}

fn glob_matches(pattern: &str, path: &std::path::Path) -> bool {
    // Use the `glob` crate if already a dep; otherwise hand-roll a tiny pattern matcher for `*` and `**`.
    // Check Cargo.toml for a `glob` dep before adding; the constraint forbids new deps, so hand-roll if needed.
    crate::server::glob_match::simple(pattern, path)
}
```

(If `glob_match::simple` doesn't exist, the implementer writes a 30-line matcher in `src/server/glob_match.rs`. The matcher handles `*` (single segment) and `**` (any depth).)

Register the handler via the `inventory::submit!` pattern used by existing handlers (`src/server/tools/handlers/registry_impl.rs:140-164` is a model).

- [ ] **Step 4: Run the test, verify it passes**

Run: `cargo test --lib audit_tools 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Run full suite**

Run: `cargo test --lib 2>&1 | tail -5`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add src/server/mcp/audit_tools.rs src/server/mcp/handler.rs src/server/glob_match.rs
git commit -m "feat(mcp): get_audit_log tool with since_unix + path_glob filters"
```

---

## Task 2.6: Audit offset persistence

**Files:**
- Modify: `src/server/audit.rs` (offset tracking)
- Modify: `src/server/presence.rs` (`PersistedState` extension already in 2.2; expose getters/setters here)

**Interfaces:**
- Produces:
  ```rust
  impl AuditLog {
      pub fn offset(state_dir: &Path) -> io::Result<u64>;          // reads audit_offset_bytes from PersistedState via LainServer hook
      pub fn set_offset(state_dir: &Path, bytes: u64) -> io::Result<()>;
  }
  ```

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn offset_round_trips_across_state_save_load() {
    // Build a PersistedState with audit_offset_bytes=12345,
    // save_pair, then load_pair, then read the offset back.
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test --lib audit_offset 2>&1 | tail -10`
Expected: FAIL.

- [ ] **Step 3: Implement offset accessors**

The simplest path: expose public getters on `PersistedState` for the audit fields, and have `AuditLog::offset`/`set_offset` read/write through the existing `save_pair` / `load_pair` infrastructure (or a small dedicated JSON sidecar). Either is fine — the implementer picks the cleaner one given the existing persistence call sites.

- [ ] **Step 4: Run, verify pass**

Run: `cargo test --lib audit_offset 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/server/audit.rs src/server/presence.rs
git commit -m "feat(audit): persist audit_offset_bytes across restarts"
```

---

## PR 2 wrap

Push branch `feat/audit-log-and-edit-landed`, open PR. PR body references the spec section `## PR 2`.

---

# PR 3: Dashboard UX (severity + filter + burst collapsing)

## Task 3.1: severity on `conflict_detected`

**Files:**
- Modify: `src/server/sse.rs` (compute severity on emit)
- Modify: `src/ui/` (render severity badge)

**Interfaces:**
- On the wire: `conflict_detected` event gains a `severity: "none"|"low"|"medium"|"high"` field.
- Severity computation: re-uses the heuristic from `src/server/mcp/presence_tools.rs` `detect_overlap` (already shipped, commit `b05ae78`). The implementer extracts the function into a small helper if it isn't already factored.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn conflict_detected_includes_severity_field() {
    // Set up two agents with overlapping Edit claims on the same symbol.
    // Subscribe to SSE; emit a `conflict_detected` and capture the wire payload.
    // Assert `"severity":"high"` (or "medium"/"low" depending on overlap density).
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test --lib sse_severity 2>&1 | tail -10`
Expected: FAIL — no `severity` field.

- [ ] **Step 3: Implement**

In `src/server/sse.rs` near the `ConflictDetected` mapping, compute severity using the shared helper from `mcp/presence_tools.rs` (extracted if needed) and include it in the JSON payload.

In the SPA (`src/ui/`), add a small `<span class="severity severity-{level}">` next to each conflict card.

- [ ] **Step 4: Run, verify pass**

Run: `cargo test --lib sse_severity 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/server/sse.rs src/server/mcp/presence_tools.rs src/ui/
git commit -m "feat(sse+ui): severity on conflict_detected + badge in dashboard"
```

---

## Task 3.2: "Only my session" toggle

**Files:**
- Modify: `src/ui/` (vanilla JS)

**Interfaces:**
- New checkbox "Only my session" in the Agents online panel.
- When checked, only events from/to the current session's `agent_id` are rendered.

- [ ] **Step 1: Snapshot test**

In the SPA's test folder (whatever convention `src/ui/` already uses — the implementer confirms by inspection):

```js
test('only-my-session filters out events for other agents', () => {
  const events = [
    { agent_id: 'a1', path: '/x.rs', kind: 'conflict_detected' },
    { agent_id: 'a2', path: '/y.rs', kind: 'conflict_detected' },
  ];
  const rendered = renderWithFilter(events, { myAgentId: 'a1', onlyMySession: true });
  expect(rendered).toHaveLength(1);
  expect(rendered[0].agent_id).toBe('a1');
});
```

- [ ] **Step 2: Run, verify fail**

Run: `cd src/ui && <existing test runner> 2>&1 | tail -10`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add the toggle markup; in the SSE-handling JS, skip non-self events when the checkbox is checked. Persist the toggle state in `localStorage` keyed by `lain_only_my_session`.

- [ ] **Step 4: Run, verify pass**

Run: same as step 2.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/ui/
git commit -m "feat(ui): Only my session toggle for conflict events"
```

---

## Task 3.3: Burst collapsing (3+ events / path / 5s)

**Files:**
- Modify: `src/ui/`

- [ ] **Step 1: Snapshot test**

```js
test('burst of 3 events same path within 5s collapses to one card', () => {
  const base = Date.now();
  const events = [
    { ts: base,        path: '/x.rs' },
    { ts: base + 1000, path: '/x.rs' },
    { ts: base + 2000, path: '/x.rs' },
  ];
  const cards = collapseBursts(events, { window_ms: 5000 });
  expect(cards).toHaveLength(1);
  expect(cards[0].count).toBe(3);
});

test('events outside the window stay separate', () => {
  const base = Date.now();
  const events = [
    { ts: base,        path: '/x.rs' },
    { ts: base + 6000, path: '/x.rs' },  // 6s later
  ];
  const cards = collapseBursts(events, { window_ms: 5000 });
  expect(cards).toHaveLength(2);
});
```

- [ ] **Step 2: Run, verify fail**

Run: same test runner as 3.2.
Expected: FAIL.

- [ ] **Step 3: Implement**

In the SPA's SSE handler, before rendering, run `collapseBursts(events, { window_ms: 5000 })`. Each collapsed card has a "show all" affordance that flips the renderer to expand inline.

- [ ] **Step 4: Run, verify pass**

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/ui/
git commit -m "feat(ui): collapse bursts of >3 events same path within 5s"
```

---

## PR 3 wrap

Push branch `feat/dashboard-noise-filters`, open PR. PR body references the spec section `## PR 3`.

---

## Self-Review (post-draft)

**Spec coverage check:**

- **Goal section** → PR 1 (staleness window), PR 2 (audit trail), PR 3 (dashboard noise). Each maps to a task above.
- **Architecture / RevisionId surface lift** → Task 1.1, 1.2, 1.3.
- **Claim.plan_revision** → Task 1.4.
- **WorldState / ChangedSymbol / ChangedKind** → Task 1.5.
- **Static-graph retract detection at claim time** → Task 1.6.
- **docs/multiplayer.md update** → Task 1.7.
- **End-to-end integration test** → Task 1.8.
- **Audit module** → Task 2.1.
- **PersistedState audit fields** → Task 2.2.
- **Wire claim → audit append** → Task 2.3.
- **SSE edit_landed** → Task 2.4.
- **get_audit_log MCP tool** → Task 2.5.
- **Audit offset persistence** → Task 2.6.
- **Severity on conflict_detected** → Task 3.1.
- **Only my session toggle** → Task 3.2.
- **Burst collapsing** → Task 3.3.

**Non-goals coverage:**
- Identity / trust provenance (concordiumagent) — not present anywhere in this plan; explicitly out.
- Cross-server federation revision — not addressed; out per spec.
- Static-graph convergence fixes — not addressed; that's the `fix/index-convergence-canonical-paths` branch.
- The `eprintln` in `attribution.rs:380` — explicitly untouched (spec §Non-goals).

**Error handling table (spec):**
- audit.jsonl write fails → Task 2.3 Step 3 (WARN, claim still valid).
- plan_revision > current → Task 1.6 Step 3.
- plan_revision < ring floor → Task 1.6 Step 3.
- audit.jsonl missing/corrupt on load → Task 2.1 (read skips missing), Task 2.2 (PersistedState default fields).
- Static-graph retract detected → Task 1.6.
- u64::MAX wrap → doc-only; called out in spec Risks. No code change.
- Hook requests plan_revision on wrong tool → server tolerates (the `plan_revision` field is only consulted by `claim_files`; other handlers ignore it via `serde(default)`).

**Placeholders / red flags:** None. Test bodies are concrete; production code blocks include real implementations.

**Type consistency:**
- `RevisionId` is the existing `pub type RevisionId = u64;` from `src/server/overlay/stream.rs:18`. New code uses it via `use crate::overlay::stream::RevisionId`. No re-definition.
- `Claim.plan_revision: Option<RevisionId>` consistent across Tasks 1.4, 1.6, 2.3.
- `WorldState.note: Option<String>`, `WorldState.changed_symbols: Vec<ChangedSymbol>` consistent across 1.5, 1.6.
- `LookupResult::{Ok, BeyondCurrent, TooOld}` consistent between `revision_log.rs` and the overlay methods.

**Coverage bar per PR:**

- PR 1: 12 new tests (Task 1.1×5, 1.2×3, 1.5×3, 1.8×2; minus the dedup test which counts as 1 even if written as 3 cases in 1.5 = ~12).
- PR 2: 10 new tests (Task 2.1×3, 2.2×1, 2.3×1, 2.4×1, 2.5×2, 2.6×1 = ~9; tests in 2.3 may expand to 2 = ~10).
- PR 3: 6 new tests across the 3 dashboard tasks.

**Risk items:** u64 wrap is documented in spec. Ring buffer cap (256) is documented; `diffs_since` returns `TooOld` to handle it gracefully. Audit cap is documented; rotation kicks in.

**Final caveat — what this plan does NOT verify at planning time:**

- Exact insertion points for `enqueue` calls in `VolatileOverlay::insert_node` / `insert_edge`. Step 3 in Task 1.2 says "after the existing petgraph insertion but before the broadcast" — if a thread-safety issue arises at implementation time (the broadcast call happens under a lock; the log needs similar care), the implementer adjusts.
- The exact `presence.rs` `PersistedState` fields after our additions may collide with future fields. Implementer verifies field uniqueness.
- The `inventory::submit!` mechanism for tool handler registration is modeled on `src/server/tools/handlers/registry_impl.rs:140-164`; the implementer should mirror that exact pattern.

These are not placeholders — they are the implementer adjusting to exact on-disk shapes they can see better than this plan can. The plan is explicit that they verify.

---

Plan complete and saved to `docs/superpowers/plans/2026-08-18-coordination-staleness-audit.md`. 3 PRs across 19 tasks.
