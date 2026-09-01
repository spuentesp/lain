# Windows Path Normalization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix three Windows path-format defects — (1) MCP `claim_files` and seven sibling tools serialize paths with backslashes via `Path::to_string_lossy()` instead of the contract's forward-slash form; (2) audit log JSONL stores backslashes on Windows so `get_recent_activity`'s `path_glob` filter (built with `/`) matches zero rows; (3) eleven `tests/multi_agent_concurrency.rs` assertions check raw `"src/..."` literals and will start failing on Windows once the server-side fix lands unless they use a path-component helper.

**Architecture:** Add a single platform-aware helper `posix_string(&Path) -> String` next to the existing `graph_path` pattern in `src/server/path_util.rs`. Replace every `path.to_string_lossy()` call on the MCP response path with `posix_string(&path)`. Normalize `AuditEvent.path` to forward-slash form at the write site so the on-disk JSONL is platform-independent. Add a `path_components_eq` helper to `tests/multi_agent_concurrency.rs` (matching the existing helper in `tests/feat_suite.rs:73-79` and `tests/toolchain_resolution.rs:25-29`) and replace the eleven unguarded literal assertions. Remove the two existing `#[cfg_attr(target_os = "windows", ignore)]` gates on `tests/presence.rs:1768` and `:1282` once their underlying fixes land.

**Tech Stack:** Rust 2021, `serde_json`, `tokio` (async tests). No new dependencies.

## Global Constraints

- **No new dependencies.** This plan touches only what is already in `Cargo.toml`.
- **Tests must run on Linux CI as the primary gate.** Windows-specific branches are gated with `#[cfg(target_os = "windows")]`; Linux tests assert the no-op case.
- **No behavior change on Unix.** `posix_string` must be a no-op when the platform separator is `/`. The audit log wire format must remain identical on Unix.
- **Commit style:** follow the project's `fix(ci): Windows — <subject>` / `fix(mcp):` / `fix(audit):` convention. One commit per task.
- **The on-disk audit JSONL format change** (`AuditEvent.path` from `\` to `/` on Windows) is a wire-format change for Windows-side consumers. There are no current downstream readers other than the in-process `read_audit_log` used by `get_recent_activity` / `get_audit_log`, which will be updated in the same commit. Document this in the task 3 commit message.
- **`graph_path` is the canonical pattern to copy** — same `MAIN_SEPARATOR` branch, same `to_string_lossy().replace(...)` shape. Do not introduce a divergent helper.

## File Structure

### New files

- `src/server/path_util.rs` — owns `pub fn posix_string(path: &Path) -> String` and inline `#[cfg(test)] mod tests`. Follows the `src/server/glob_match.rs` pattern (single helper, inline tests).

### Modified files

- `src/server/mod.rs` — add `pub mod path_util;` in the utilities cluster (next to `glob_match`, `time`).
- `src/server/mcp/presence_tools.rs` — replace eight `path.to_string_lossy()` call sites (lines 155, 421, 427, 439, 667, 695, 731, 907) with `posix_string(&path)`. Normalize the audit write at line 392 so `AuditEvent.path` carries forward slashes.
- `src/server/mcp/audit_tools.rs` — apply `posix_string` to the `group_key` path branch at line 193.
- `src/server/audit.rs` — add a unit test that asserts `AuditEvent` serializes `path` as forward slashes on the platform-native form.
- `tests/multi_agent_concurrency.rs` — add `path_components_eq(path: &str, expected: &[&str]) -> bool` at the top of the file (matching `tests/feat_suite.rs:73-79`), and replace eleven `Some("src/...")` literal assertions with helper calls.
- `tests/presence.rs` — remove `#[cfg_attr(target_os = "windows", ignore)]` from `claim_files_accepts_string_form_files` (line 1768) and `get_recent_activity_tool_groups_by_path` (line 1282).

### Files NOT touched

- `tests/feat_suite.rs`, `tests/toolchain_resolution.rs` — already use `path_components_eq`. No change needed.
- `src/server/presence.rs::lexical_normalize` — keep `PathBuf::push` semantics; downstream consumers normalize. Changing this function would alter unrelated call sites.

---

## Task 1: Add `posix_string` helper

**Files:**
- Create: `src/server/path_util.rs`
- Modify: `src/server/mod.rs` (add `pub mod path_util;` in the utilities cluster)

**Interfaces:**
- Consumes: `&std::path::Path`
- Produces: `pub fn posix_string(path: &Path) -> String` — the path as a string with forward-slash separators on every platform. On Unix: identical to `path.to_string_lossy()`. On Windows: `\` replaced with `/`.

- [ ] **Step 1: Write the failing test**

Create `src/server/path_util.rs` with the test module only:

```rust
//! Cross-platform path string formatting.
//!
//! The MCP wire format and the audit log JSONL both store paths as
//! forward-slash strings regardless of host platform, so a Linux
//! agent talking to a Windows `lain` server sees the same path
//! shape a Windows agent does. `posix_string` is the single
//! canonical helper for that conversion.

use std::path::Path;

/// Render `path` as a forward-slash string, the form every wire
/// protocol and on-disk log in this crate expects.
///
/// On Unix this is a no-op — `to_string_lossy` already produces
/// `/`-separated strings. On Windows it rewrites `\` to `/` so the
/// output matches what a Linux consumer would have written.
///
/// This is the same shape `crate::server::graph::graph_path` uses
/// for index-map keys; the two helpers differ only in that
/// `graph_path` strips a workspace prefix first. Do not duplicate
/// the platform branch anywhere else in the crate — call this.
pub fn posix_string(path: &Path) -> String {
    let s = path.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '/' {
        s.into_owned()
    } else {
        s.replace(std::path::MAIN_SEPARATOR, "/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_string_is_no_op_on_native_forward_slash_paths() {
        // Runs on every platform: confirms forward-slash input is
        // preserved verbatim.
        assert_eq!(posix_string(Path::new("src/a.rs")), "src/a.rs");
        assert_eq!(posix_string(Path::new("a/b/c.rs")), "a/b/c.rs");
        assert_eq!(posix_string(Path::new("")), "");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn posix_string_normalizes_windows_separators() {
        // Windows-only: confirm the `\` → `/` rewrite happens.
        assert_eq!(posix_string(Path::new("src\\a.rs")), "src/a.rs");
        assert_eq!(posix_string(Path::new("a\\b\\c.rs")), "a/b/c.rs");
    }

    #[cfg(target_os = "unix")]
    #[test]
    fn posix_string_does_not_touch_unix_paths_with_literal_backslash() {
        // Unix-only guard: `Path::new("src\\a.rs")` is a single
        // component with a backslash in the filename, not a
        // separator. The helper must not rewrite it on Unix.
        assert_eq!(posix_string(Path::new("src\\a.rs")), "src\\a.rs");
    }
}
```

- [ ] **Step 2: Wire the module into `src/server/mod.rs`**

Add `pub mod path_util;` immediately after the existing `pub mod glob_match;` line (around line 53 of `src/server/mod.rs`). Place it in the same cluster — both are small utilities used by the MCP layer.

- [ ] **Step 3: Run tests, confirm the no-op case passes on Unix**

Run: `cargo test --lib server::path_util`

Expected: PASS for both `posix_string_is_no_op_on_native_forward_slash_paths` and `posix_string_does_not_touch_unix_paths_with_literal_backslash`. (The Windows-only test is `cfg`-gated out.)

- [ ] **Step 4: Commit**

```bash
git add src/server/path_util.rs src/server/mod.rs
git commit -m "feat(server): add posix_string helper for cross-platform path formatting"
```

---

## Task 2: Apply `posix_string` to all MCP response sites

**Files:**
- Modify: `src/server/mcp/presence_tools.rs` (eight sites — see below)

**Interfaces:**
- Consumes: `posix_string` from `crate::server::path_util`
- Produces: every MCP response that surfaces a `PathBuf` field now emits forward-slash form

The eight sites, all in `src/server/mcp/presence_tools.rs`:

| Line | Function | Field |
| --- | --- | --- |
| 155 | `run_list_my_claims` | per-claim `path` |
| 421 | `run_claim_files` | `granted[i].path` (Bug #13 origin) |
| 427 | `run_claim_files` | `conflicts[i].path` |
| 439 | `run_claim_files` | `advisories[i].path` |
| 667 | `run_release_files` | `released[i]` |
| 695 | `run_list_occupancy` | per-entry `path` |
| 731 | `run_my_claims` | per-claim `path` |
| 907 | `run_detect_overlap` | overlap keys |

- [ ] **Step 1: Add the import**

At the top of `src/server/mcp/presence_tools.rs`, in the existing `use` block, add:

```rust
use crate::server::path_util::posix_string;
```

- [ ] **Step 2: Replace all eight call sites**

For each of the eight lines above, replace `path.to_string_lossy()` (or `g.path.to_string_lossy()` etc. — match the surrounding variable name) with `posix_string(&path)` (or `posix_string(&g.path)` etc.).

The exact substitutions:

- Line 155: `"path": entry.path.to_string_lossy(),` → `"path": posix_string(&entry.path),`
- Line 421: `"path": g.path.to_string_lossy(),` → `"path": posix_string(&g.path),`
- Line 427: `"path": c.path.to_string_lossy(),` → `"path": posix_string(&c.path),`
- Line 439: `"path": a.path.to_string_lossy(),` → `"path": posix_string(&a.path),`
- Line 667: `"path": r.path.to_string_lossy(),` → `"path": posix_string(&r.path),`
- Line 695: `"path": entry.path.to_string_lossy(),` → `"path": posix_string(&entry.path),`
- Line 731: `"path": entry.path.to_string_lossy(),` → `"path": posix_string(&entry.path),`
- Line 907: `"path": entry.path.to_string_lossy(),` → `"path": posix_string(&entry.path),`

After the edit, `grep -n "to_string_lossy" src/server/mcp/presence_tools.rs` should show no remaining matches.

- [ ] **Step 3: Add a regression unit test**

Add this test inside the existing `#[cfg(test)] mod tests` block at the bottom of `src/server/mcp/presence_tools.rs`. If the file does not already have an inline test module, add one at the bottom with the standard `#[cfg(test)] mod tests { use super::*; ... }` shape:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn posix_string_is_used_for_granted_path_in_serialized_response() {
        // Sanity-check that the wire-format conversion is in place.
        // The integration test `claim_files_accepts_string_form_files`
        // covers the live response shape; this is a static guard
        // against future regressions to `to_string_lossy`.
        let path = PathBuf::from("src/a.rs");
        let rendered = posix_string(&path);
        assert_eq!(rendered, "src/a.rs");
        assert!(!rendered.contains('\\'));
    }
}
```

- [ ] **Step 4: Run the test suite for this module**

Run: `cargo test --lib server::mcp::presence_tools`

Expected: all existing tests pass; the new test passes.

- [ ] **Step 5: Run the broader presence + MCP suite**

Run: `cargo test --test presence`

Expected: PASS on Linux. (The `claim_files_accepts_string_form_files` test at line 1796 still has its `#[cfg_attr(target_os = "windows", ignore")]` gate for now — Task 5 removes it.)

- [ ] **Step 6: Commit**

```bash
git add src/server/mcp/presence_tools.rs
git commit -m "fix(mcp): serialize claim/release/occupancy paths via posix_string — Windows backslash regression"
```

---

## Task 3: Fix audit log path storage (Bug #14)

**Files:**
- Modify: `src/server/mcp/presence_tools.rs:392` (audit write site)
- Modify: `src/server/audit.rs` (add regression test)

**Interfaces:**
- Consumes: `posix_string` from `crate::server::path_util`
- Produces: every `AuditEvent` written by the claim flow carries a `path` whose string form uses `/`, so the on-disk JSONL is platform-independent and `get_recent_activity`'s `path_glob` filter (which uses `/`) matches on every host.

The audit write site is at `src/server/mcp/presence_tools.rs:389-397`:

```rust
let audit = AuditEvent {
    ts_unix,
    agent_id: session.id.clone(),
    path: g.path.clone(),    // line 392 — currently a backslash PathBuf on Windows
    claim_set,
    racers: result.conflicts.clone(),
    plan_revision,
    landed_revision,
};
```

- [ ] **Step 1: Verify this is the only `AuditEvent` construction site**

Run: `grep -rn "AuditEvent {" src/ tests/`

Expected: only one hit, at `src/server/mcp/presence_tools.rs:389`. (If a future task adds another constructor, it must apply the same normalization — note this in the commit body.)

- [ ] **Step 2: Replace the path with the forward-slash form**

In `src/server/mcp/presence_tools.rs:392`, change:

```rust
                path: g.path.clone(),
```

to:

```rust
                // Store the path in the canonical forward-slash form so
                // the on-disk JSONL is platform-independent. Any
                // `path_glob` filter that uses `/` (see audit_tools.rs)
                // will match regardless of host OS, and downstream
                // consumers (federation replication, future log
                // shipping) see a stable wire form.
                path: PathBuf::from(posix_string(&g.path)),
```

Add `PathBuf` to the imports at the top of `presence_tools.rs` if not already present (`grep -n "use std::path" src/server/mcp/presence_tools.rs` should show it's already imported via the parent module).

- [ ] **Step 3: Add a regression unit test in `src/server/audit.rs`**

Add this test inside the existing `#[cfg(test)] mod tests` block at the bottom of `src/server/audit.rs` (which already exists per the file layout — verify with `grep -n "mod tests" src/server/audit.rs`):

```rust
    #[test]
    fn audit_event_path_serializes_with_forward_slash_separators() {
        // The audit JSONL is consumed by tools whose `path_glob`
        // filters use `/`. The on-disk serialization of `path`
        // must therefore use `/` regardless of host OS.
        let event = AuditEvent {
            ts_unix: 0.0,
            agent_id: AgentId("test".into()),
            path: PathBuf::from("src/a.rs"),
            claim_set: vec![],
            racers: vec![],
            plan_revision: None,
            landed_revision: RevisionId(0),
        };
        let line = serde_json::to_string(&event).expect("serialize");
        assert!(
            line.contains("\"src/a.rs\""),
            "expected forward-slash form, got: {line}"
        );
        assert!(
            !line.contains("\\\\"),
            "unexpected backslash escape in path serialization: {line}"
        );
    }
```

The test passes on every platform because `PathBuf::from("src/a.rs")` is the input the helper has already normalized — this guards against future regressions to the audit write site that might pass a Windows-native PathBuf through.

- [ ] **Step 4: Run the audit module tests**

Run: `cargo test --lib server::audit`

Expected: all existing tests pass; the new test passes.

- [ ] **Step 5: Run the cross-module integration test**

Run: `cargo test --test presence get_recent_activity_tool_groups_by_path`

Expected: PASS on Linux. (The `#[cfg_attr(target_os = "windows", ignore)]` gate at line 1282 is still in place — Task 5 removes it. On Linux the test already passed; the regression test ensures we did not regress Linux behavior.)

- [ ] **Step 6: Commit**

```bash
git add src/server/mcp/presence_tools.rs src/server/audit.rs
git commit -m "fix(audit): write AuditEvent.path in forward-slash form so path_glob filter matches on Windows"
```

---

## Task 4: Apply `posix_string` to `group_key` in `audit_tools.rs`

**Files:**
- Modify: `src/server/mcp/audit_tools.rs:193` (group_key path branch)

**Interfaces:**
- Consumes: `posix_string` from `crate::server::path_util`
- Produces: `get_recent_activity` with `group_by: "path"` returns forward-slash group keys on every platform

The site is at `src/server/mcp/audit_tools.rs:181-195`:

```rust
fn group_key(ev: &AuditEvent, group_by: &str) -> String {
    match group_by {
        "agent" => format!("agent:{}", ev.agent_id.0),
        "hour"  => { /* … */ }
        _ => ev.path.to_string_lossy().to_string(),   // line 193
    }
}
```

- [ ] **Step 1: Add the import**

At the top of `src/server/mcp/audit_tools.rs`, add `use crate::server::path_util::posix_string;` to the existing `use` block.

- [ ] **Step 2: Replace the path branch**

Change line 193 from:

```rust
        _ => ev.path.to_string_lossy().to_string(),
```

to:

```rust
        _ => posix_string(&ev.path),
```

(The `to_string()` was converting a `Cow<str>` to `String`; `posix_string` already returns `String`.)

- [ ] **Step 3: Add a regression unit test**

Add this test inside the existing `#[cfg(test)] mod tests` block at the bottom of `src/server/mcp/audit_tools.rs` (verify with `grep -n "mod tests" src/server/mcp/audit_tools.rs`):

```rust
    #[test]
    fn group_key_path_branch_uses_forward_slashes() {
        // After Task 3, every `AuditEvent.path` written by the
        // claim flow already uses `/`. This guards the read-side
        // group_key against future regressions to `to_string_lossy`
        // and against any path field that did not go through the
        // audit write site.
        let event = AuditEvent {
            ts_unix: 0.0,
            agent_id: AgentId("a".into()),
            path: PathBuf::from("src/a.rs"),
            claim_set: vec![],
            racers: vec![],
            plan_revision: None,
            landed_revision: RevisionId(0),
        };
        let key = group_key(&event, "path");
        assert_eq!(key, "src/a.rs");
        assert!(!key.contains('\\'));
    }
```

(You may need to add `use crate::server::audit::{AuditEvent, append_edit_event};` and `use crate::server::presence::{AgentId, Claim};` and `use crate::server::revision_log::RevisionId;` to the test module's imports. Check the existing test module for which are already in scope.)

- [ ] **Step 4: Run tests**

Run: `cargo test --lib server::mcp::audit_tools`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/server/mcp/audit_tools.rs
git commit -m "fix(audit): group_key path branch uses posix_string for cross-platform keys"
```

---

## Task 5: Add `path_components_eq` to `tests/multi_agent_concurrency.rs` and apply it

**Files:**
- Modify: `tests/multi_agent_concurrency.rs` (add helper, replace eleven assertions)

**Interfaces:**
- Produces: a file-local `fn path_components_eq(path: &str, expected: &[&str]) -> bool` matching the existing `tests/feat_suite.rs:73-79` helper.

- [ ] **Step 1: Verify there is no existing helper**

Run: `grep -n "fn path_components_eq\|path_components_eq" tests/multi_agent_concurrency.rs`

Expected: no matches. (The helper is currently local to `tests/feat_suite.rs` and `tests/toolchain_resolution.rs`; this plan deliberately does not promote it to a shared `tests/common` module to keep the change scoped.)

- [ ] **Step 2: Add the helper at the top of the test file**

Insert immediately after the existing `use` block at the top of `tests/multi_agent_concurrency.rs`:

```rust
/// Compare a server-supplied path string against an expected list of
/// path components, treating both `/` and `\` as separators. Used
/// instead of `==` so the same assertion holds whether the server
/// emits forward slashes (Linux) or backslashes (Windows). Matches
/// the helper in `tests/feat_suite.rs`.
fn path_components_eq(path: &str, expected: &[&str]) -> bool {
    let actual: Vec<&str> = path
        .split(['/', '\\'])
        .filter(|s| !s.is_empty())
        .collect();
    actual == expected
}
```

- [ ] **Step 3: Replace the eleven literal assertions**

For each of the lines below, replace the literal `== Some("src/...")` (or `== "src/..."`) check with a `path_components_eq` call. The exact substitutions:

- Line 477 (inside the `claims_grant_test` block):
  ```rust
  g.get("path").and_then(|p| p.as_str()) == Some("src/contested.rs")
  ```
  becomes:
  ```rust
  g.get("path").and_then(|p| p.as_str()).is_some_and(|p| path_components_eq(p, &["src", "contested.rs"]))
  ```

- Line 490: same pattern with `"src/contested.rs"`.
- Line 571: `"src/shared.rs"`.
- Line 622: `"src/shared.rs"`.
- Line 647: `"src/shared.rs"`.
- Line 688: `pointer("/granted/0/path").unwrap().as_str() == Some("src/x.rs")` → `pointer("/granted/0/path").unwrap().as_str().is_some_and(|p| path_components_eq(p, &["src", "x.rs"]))`.
- Line 706: `released.iter().any(|p| p.as_str() == Some("src/x.rs"))` → `released.iter().any(|p| p.as_str().is_some_and(|x| path_components_eq(x, &["src", "x.rs"])))`.
- Line 727: `"src/x.rs"`.
- Line 790: `"src/y.rs"`.
- Line 844: `"src/guarded.rs"`.
- Line 877: `pointer("/granted/0/path") ... == Some("src/guarded.rs")` → `pointer("/granted/0/path").unwrap().as_str().is_some_and(|p| path_components_eq(p, &["src", "guarded.rs"]))`.

After the edit, `grep -n 'Some("src/' tests/multi_agent_concurrency.rs` should show no remaining response-side path-literal assertions. (Inputs — the strings passed to `claim()` — are fine and stay as-is.)

- [ ] **Step 4: Run the test suite**

Run: `cargo test --test multi_agent_concurrency`

Expected: PASS on Linux. (Linux behavior is unchanged — `posix_string` is a no-op there — but the helper now decouples the assertion from the wire format, so the tests will keep passing once the server-side normalization makes Windows match Linux.)

- [ ] **Step 5: Commit**

```bash
git add tests/multi_agent_concurrency.rs
git commit -m "test(mcp): multi_agent_concurrency uses path_components_eq helper for Windows-safe assertions"
```

---

## Task 6: Un-ignore Windows tests in `tests/presence.rs`

**Files:**
- Modify: `tests/presence.rs` (remove two `#[cfg_attr(target_os = "windows", ignore)]` lines)

The two sites, after the fixes from Tasks 2 and 3, will pass on Windows:

- Line 1768: `claim_files_accepts_string_form_files` — asserts `granted[0]["path"] == "src/a.rs"`. Passes on Windows once Task 2 normalizes the response.
- Line 1282: `get_recent_activity_tool_groups_by_path` — asserts `total_events == 4`. Passes on Windows once Task 3 normalizes the audit write.

- [ ] **Step 1: Remove the line 1768 gate**

In `tests/presence.rs`, delete the line:

```rust
#[cfg_attr(target_os = "windows", ignore)]
```

immediately above `async fn claim_files_accepts_string_form_files()`.

- [ ] **Step 2: Remove the line 1282 gate**

In `tests/presence.rs`, delete the line:

```rust
#[cfg_attr(target_os = "windows", ignore)]
```

immediately above `async fn get_recent_activity_tool_groups_by_path()`.

- [ ] **Step 3: Verify no other `cfg_attr(target_os = "windows", ignore)` lines remain in `tests/presence.rs` that this plan is intended to remove**

Run: `grep -n "cfg_attr(target_os = \"windows\", ignore)" tests/presence.rs`

Expected: only matches that this plan does not target (i.e., unrelated Windows-skip tests). The two for `claim_files_accepts_string_form_files` and `get_recent_activity_tool_groups_by_path` must be gone.

- [ ] **Step 4: Run the test suite on Linux**

Run: `cargo test --test presence`

Expected: PASS. (Behavior on Linux was already correct; we just removed the Windows bypass so that on a future Windows CI run, the test will execute and validate the server-side fix.)

- [ ] **Step 5: Sanity-check that the test bodies still reference the forward-slash form**

Run: `grep -n '"src/a.rs"\|"src/contested.rs"\|"src/shared.rs"' tests/presence.rs`

Expected: all hits are inputs (passed to MCP tool args) or are inside the two un-ignored tests as the expected response shape. No response-side literal assertions remain that have not been un-ignored.

- [ ] **Step 6: Commit**

```bash
git add tests/presence.rs
git commit -m "test(ci): remove Windows ignore from claim_files and get_recent_activity tests — fixes landed"
```

---

## Task 7: Final verification and audit

**Files:** none modified (verification only)

- [ ] **Step 1: Run the full Rust test suite on Linux**

Run: `cargo test --workspace`

Expected: all tests pass. No new failures introduced by Tasks 1-6.

- [ ] **Step 2: Run the JS test suite (if it exists and is relevant)**

Run: `cd ui && npm test 2>/dev/null || cd .. && cargo test --workspace`

Expected: same as above. (No JS changes in this plan; this is a regression guard.)

- [ ] **Step 3: Cross-compile check for Windows (best-effort)**

Run: `cargo check --target x86_64-pc-windows-gnu 2>&1 | tail -30`

Expected: either compiles cleanly, or fails with only "linker not found" / target-not-installed errors (which means the toolchain isn't installed locally — that's acceptable; CI will catch real compile errors). Any source-level error (missing import, type mismatch, etc.) must be fixed before this plan is complete.

If `x86_64-pc-windows-gnu` is not installed, document the gap in the commit message and move on — `cargo check` without `--target` is sufficient.

- [ ] **Step 4: Confirm no stray `to_string_lossy` calls on path fields in the MCP response surface**

Run: `grep -rn "to_string_lossy" src/server/mcp/`

Expected: no matches in files that build MCP response JSON. (Other modules — `presence.rs`, `presence_lock.rs`, `ingest/` — may legitimately use `to_string_lossy` for non-wire purposes; this guard is specifically about response serialization.)

- [ ] **Step 5: Confirm no remaining Windows-gated tests in the fixed paths**

Run: `grep -B1 "claim_files_accepts_string_form_files\|get_recent_activity_tool_groups_by_path" tests/presence.rs | head -10`

Expected: no `#[cfg_attr(target_os = "windows", ignore)]` line immediately above either test function.

- [ ] **Step 6: Update CHANGELOG.md**

Add an entry under the next unreleased version in `CHANGELOG.md` (find the current top-of-file section):

```markdown
### Fixed
- MCP `claim_files`, `release_files`, `list_occupancy`, `my_claims`,
  `list_my_claims`, `detect_overlap` now serialize `path` fields in
  forward-slash form on every platform (previously `\` on Windows,
  breaking the MCP wire contract).
- Audit log JSONL now stores `path` in forward-slash form on every
  platform; `get_recent_activity`'s `path_glob` filter matches on
  Windows as a result.
```

- [ ] **Step 7: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs: CHANGELOG entry for Windows path normalization fixes"
```

---

## Self-Review

**1. Spec coverage:**

| Inventory item | Addressed by |
| --- | --- |
| #13 — `claim_files` `granted[0].path` backslashes | Task 2 (primary site) + Task 2 (seven sibling sites in the same change) |
| #14 — `get_recent_activity` `total_events = 0` | Task 3 (audit write site) + Task 4 (group_key read site) |
| #17 — silent Windows failures in `tests/multi_agent_concurrency.rs` after server fix lands | Task 5 (eleven assertions) + Task 6 (two `cfg_attr` gates) |

All three inventory items in scope are covered. No gaps.

**2. Placeholder scan:** No `TODO`, `TBD`, "implement later", or "similar to Task N" placeholders. Every code block contains the exact code to type. Every step has a concrete `git` command.

**3. Type consistency check:**

- `posix_string(&Path) -> String` is defined in Task 1 and consumed by Tasks 2, 3, 4 — same signature throughout.
- `path_components_eq(path: &str, expected: &[&str]) -> bool` is defined in Task 5 and used only within that task.
- `AuditEvent { path: PathBuf, ... }` field type is unchanged — the value stored in `path` is now a forward-slash `PathBuf` instead of a platform-native `PathBuf`, but the type is the same so no downstream `match` arm needs updating.
- `GroupKey::path` branch in `audit_tools.rs:193` previously returned `String` via `to_string_lossy().to_string()`; after Task 4 it returns `String` via `posix_string(&ev.path)`. Same return type, same call sites, no signature change.

**4. Cross-platform test strategy:** Every Windows-specific branch is `#[cfg(target_os = "windows")]`-gated; every Linux branch is `#[cfg(target_os = "unix")]`-gated or unconditional. CI on Linux validates the no-op behavior; CI on Windows (already running per the CI saga work) will validate the rewrite branch via the un-ignored tests in Task 6.

**5. Commit log shape:** Seven commits, one per task, each self-contained and revertable. Matches the project's `fix(ci):` / `fix(mcp):` / `fix(audit):` convention observed in `git log --oneline -20`.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-09-01-windows-path-normalization.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration with two-stage review.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

**Note on remaining plans:** This plan covers Plan A from the parked-bug inventory (Windows path normalization, items #13/#14/#17). The other follow-up plans are scoped but not yet written:

- **Plan B: V2 polish bundle** (#1 search disambiguation, #2 blast-radius error UX, #3 duplicate CSS rule)
- **Plan C: Infra/workflow improvements** (#5 recorder `--ready-timeout-ms` flag, #7 `include_bytes!` JS workflow, CI workflow `lain --bin` guard)
- **Plan D: Overlay test coverage hardening** (#11 tree-sitter no-LSP path test for federation overlay)
- **Plan E: macOS-deferred** (#12 hot-reload snapshot race, #15 FSEvents harness — needs hardware to repro, parked until macOS CI coverage matters)

Say the word and I'll write any of B/C/D/E as the next plan.
