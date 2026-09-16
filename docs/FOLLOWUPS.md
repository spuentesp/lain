# Followups — the active backlog

This is a living backlog of work deferred from the SOLID/DRY audit
(PRs 6a–6f) plus a handful of pre-existing issues surfaced along the
way. Each item carries enough context that an agent can pick it up
without re-reading the audit report.

Conventions:

- **Phase N.M** — refers to a numbered item in
  [`docs/ARCHITECTURE.md`](ARCHITECTURE.md#code-architecture-invariants-solid--dry)
  or the audit summary in the PR descriptions.
- **Severity** — `low` / `med` / `high` — describes how badly it blocks
  future work, not how complex the fix is.
- **Files** — concrete paths so the work doesn't start with a grep.

Each item has its own checklist so a single agent can claim it,
land it, and check the box without coordinating with anyone.

---

## 3.7 — Split `LainServer` into concern-bounded handles

**Severity: high** (next federation consumer will need this)

The `LainServer` struct at `src/server/ingest/server.rs:41-216` is a
30-field god struct partitioned into 9 concerns (Ingest, Refresh,
Federation, Presence, Audit, Hot reload, Auth, Attribution,
Lifecycle). Phase 1.5 added section comments; the structural split
itself is deferred per the plan's recommendation ("the single
riskiest change; defer until after PR 4 confirms the dispatch split
works").

**Files**

- `src/server/ingest/server.rs:41-216` — field partition is documented
  in section comments; the field-grouping is the migration map.
- `src/server/ingest/handles.rs` (new) — owns the handles
  (`LainIngestHandle`, `FederationHandle`, `PresenceLayer`,
  `RefreshState`, `HotReloadBus`, `AttributionState`).
- Every `LainServer` caller — `cli/server.rs`, `mcp/handler.rs`,
  `tests/workspace_e2e.rs`, `tests/use_cases/workspace_graph_peers.rs`.

**Shape (proposed, not final — run `rust-router` first to validate)**

```rust
pub struct LainServer {
    ingest: LainIngestHandle,
    federation: FederationHandle,
    presence: PresenceLayer,
    refresh: RefreshState,
    hot_reload: HotReloadBus,
    attribution: AttributionState,
    auth: Arc<AuthState>,
    events_log: Arc<EventsLog>,
    started_at: SystemTime,
}
```

`LainIngestHandle` carries `graph`, `overlay`, `embedder`,
`cross_encoder`, `git`, `lsp_pool`, `tool_executor`, `tuning`,
`id_namespace`, `overlay_paths`, `process_change_lock`,
`overlay_updated`, `overlay_revision`, `config`, plus the existing
field-by-field accessor methods. `FederationHandle`, `PresenceLayer`,
etc. follow the same pattern.

**Acceptance**

- `cargo check` clean, `cargo test --lib` 863/863.
- The 47 methods on `LainServer::impl` move to their respective
  handle's `impl` block. Public accessors on `LainServer` keep the
  same signature so callers don't change.
- `LainServer::clone` stays `Clone` (the handles are `Arc`-backed
  internally).

**Pre-flight before starting**

1. Load `m04-zero-cost` to confirm the trait-object vs
   generic-handle trade-off (the plan recommends concrete handles,
   not trait objects).
2. Load `rust-skill-creator` for the per-handle module
   declarations.
3. Read `docs/ARCHITECTURE.md#code-architecture-invariants` once more
   so the split aligns with the field-grouping comment markers.

**Do not start before**

- 3.2 followup (below) is done. Splitting LainServer while
  federation/workspace tools are still on the match arm makes the
  diff unreadable.

---

## 3.2 followup — migrate federation + workspace tools to inventory

**Severity: med** (reduces the dispatch match by ~150 more lines;
unblocks 3.7)

Phase 3.2 (PR 6f) migrated 17 of the 26 special-case tools
(presence, audit, status, reload) to the inventory sub-registry.
Federation (`list_repos`, `get_repo_info`, `get_federation_health`,
`search_org`, `get_cross_repo_blast_radius`,
`get_cross_repo_blast_radius_for_repo`) and workspace
(`list_workspaces`, `get_active_workspace`, `get_workspace`,
`get_workspace_graph`) are still on match arms in
`src/server/mcp/handler.rs:dispatch_tool_call`.

**Files**

- `src/server/mcp/handler.rs:dispatch_tool_call` — the second
  `match name` arm block (after the federation check, before
  `if let Some(fed) = federation { ... }`).
- `scripts/check-mcp-dispatch-shape.py:KNOWN_DISPATCH_ARMS` —
  shrink the list from 10 to 0 once both sub-trees migrate.
- `src/server/mcp/federation_tools/{federation,workspace}.rs` — add
  `declare_federation_tool!` / `declare_workspace_tool!` macro
  invocations next to each tool's existing free function.

**Pattern (already used for presence tools)**

```rust
declare_federation_tool!(list_repos_handler, "list_repos",
    |fed, _args| crate::server::mcp::federation_tools::list_repos(fed));
```

Add a `declare_federation_tool!` / `declare_workspace_tool!` macro
mirroring `declare_presence_tool!` (see
`src/server/mcp/handler.rs` for the existing macro).

**Acceptance**

- `cargo check` clean, 863 lib tests pass.
- `dispatch_tool_call` becomes a 4-line inventory iteration
  followed by the federation/workspace match arms being deleted.
- `KNOWN_DISPATCH_ARMS` shrinks to `set()` and the
  `ARM_GROWTH_BUDGET` becomes `0`.

---

## 3.1 walker extraction — sensors/util.rs

**Severity: low** (cosmetic; doesn't change behavior)

After Phase 3.1 (PR 6e), each of the 5 sensors still has its own
copy of the `ignore::WalkBuilder::new(root).hidden(true).git_ignore(true)
...` walker shell, plus private copies of `to_snake_case`,
`to_camel_case`, and the `find_handler` resolver. The audit's
finding H2 was that these are duplicated; Phase 3.1 migrated the
*registration* to the trait pattern but left the body duplication
intact.

**Files**

- `src/server/sensors/util.rs` (new) — owns:
  - `pub fn walk_protocol_files(root, extensions: &[&str]) -> impl Iterator<Item = PathBuf>`
  - `pub fn to_snake_case(name: &str) -> String`
  - `pub fn to_camel_case(name: &str) -> String`
  - `pub fn resolve_handler(graph, name, scope_path) -> Option<GraphNode>`
- `src/server/sensors/{proto,openapi,graphql,http,websocket}_sensor.rs`
  — replace local walkers with `walk_protocol_files(root, &[...])`
  and local helpers with `util::to_snake_case` / `util::resolve_handler`.

**Acceptance**

- 5 sensor files each shrink by 20-40 lines.
- `cargo check --lib -p sensors` clean.
- Mutation check `scripts/mutation-check.py` still ≥13/17.

**Note**: `http_sensor.rs` is the odd one (it scans `["rs", "py", "ts", "js", "go"]` instead of `["proto"]` or `["graphql"]`); the helper signature takes `&[&str]` so each sensor passes its own extension list.

---

## `tests/feat_negative_paths.rs:218` — pre-existing E0382

**Severity: low** (test bug, doesn't affect production)

```
tests/feat_negative_paths.rs:218:13: error[E0382]: use of moved value: `project`
tests/feat_negative_paths.rs:218:22: error[E0382]: use of moved value: `state`
tests/feat_negative_paths.rs:218:29: error[E0382]: use of moved value: `xdg_config`
```

Three `use of moved value` errors at the same line. Confirmed via
`git stash` to pre-date this work — it was broken before any of the
audit-driven refactors started.

**File**: `tests/feat_negative_paths.rs:218`

**Likely cause**: a closure captures `project`, `state`, `xdg_config`
by move, then the test reuses the variables outside the closure.
Fix is either to capture by reference or to clone before the
closure.

---

## Federation repo-mapping duplication (audit L2)

**Severity: low** (correctness-neutral; the helpers already
converge)

`src/server/mcp/handler.rs` and `src/server/mcp/federation_tools/federation.rs`
each have their own `fed.list_repos().into_iter().find(...)`
pattern to look up a repo by id. `FederatedIndex::find_repo(&RepoId)`
already exists in
`src/server/federation/federated_index.rs` and does the same lookup.

**Files**

- `src/server/mcp/handler.rs` — the `get_repo_info` arm and
  `get_cross_repo_blast_radius*` arms.
- `src/server/mcp/federation_tools/federation.rs:list_repos` /
  `get_repo_info` / `search_org`.

**Fix**: replace the `find` with `fed.find_repo(&rid)` and let
`find_repo` return the `(RepoHealth, &RepoIndex)` tuple. Audit
finding L2 also calls out that `get_repo_info` does its own
re-do of `list_repos()` to find the health — the new method
returns both, so this collapses naturally.

---

## HandlerStatus duplicate field-counting (audit M6)

**Severity: low**

`HandlerStatus::render` (in `src/server/mcp/handler.rs`) is
constructed at four sites with subtly divergent field counts —
`handler.rs:268-303` (struct), `handler.rs:940-956` (stdio),
`handler.rs:1290-1298` (sidecar), `handler.rs:1477-1491` (HTTP),
plus `federation_tools/server_status.rs:32-65`. The audit
called out that the sidecar sets `repo_count: 0,
workspaces_count: 0` because the accessor path was different.

**Fix**: a single `HandlerStatus::from_lain_server(server:
&LainServer, transport, port) -> HandlerStatus` that reads through
the canonical `LainServer::repo_count()` /
`LainServer::workspace_count()` accessors. After 3.7 (LainServer
split), `repo_count` and `workspace_count` move into
`FederationHandle` and the constructor reads from there.

---

## CLI duplicate `now → unix` helpers (audit L1 / M8)

**Severity: low**

Four CLI files each have their own "now → unix timestamp" helper:

- `src/cli/setup.rs:370-374` (`backup_file`)
- `src/cli/setup.rs:1014-1017` (second `backup_file`-shape)
- `src/cli/hooks.rs:293-298` (`fn chrono_now_unix`)
- `src/cli/mcp.rs:249-252` (nanoseconds variant)

`src/server/time.rs` already has the canonical `unix_secs`,
`unix_secs_u64`, `now_unix`, `now_unix_f64`. The CLI helpers
should `use crate::server::time::unix_secs_u64` (or the
nanosecond variant) and delete the local copies.

**Acceptance**: zero `chrono_now_unix` references; the four files
import from `crate::server::time`.

---

## `federation_blob` duplicate JSON projection (audit M3)

**Severity: low**

`src/server/mcp/handler.rs:1563-1582` (`federation_blob`) and
`src/server/mcp/federation_tools/federation.rs:list_repos` both
build a JSON projection of the federation's repos. The audit's
note is that the two paths independently agree on the
`200-byte-per-node + 100-byte-per-edge` memory formula — a magic
number duplicated as a literal in both files.

**Fix**: extract `FederationHandle::memory_estimate_bytes(nodes:
u64, edges: u64) -> u64` and have both call sites use it. After
3.7 lands, this becomes a method on `FederationHandle`.

---

## Federation re-keying `list_repos().find()` pattern (audit L2)

**Severity: low** (subsumed by `FederatedIndex::find_repo` once
that one is wired through)

Covered by the federation repo-mapping followup above; listed
separately because the audit also called it out in
`src/server/mcp/federation_tools/workspace.rs:50-54`,
`workspace.rs:92-95`, and `workspace.rs:148-152` (three separate
`list_repos()` walks in the same file).

---

## `dto::WorkspaceGraph` still in `federation_tools/dto.rs`

**Severity: low** (intentional, but worth noting)

Phase 3.3 (PR 6d) deleted `dto::GraphNode` and `dto::GraphEdge`
because they mirrored `schema::*`. `dto::WorkspaceGraph` stayed
because it's a response envelope (`{nodes, edges, truncated}`),
not a mirror — the audit explicitly carved this out.

`dto.rs` still holds 9 federation-specific DTOs (`RepoInfo`,
`FederationHealth`, `SymbolMatch`, `CrossRepoBlastRadius`,
`WorkspaceInfo`, `ActiveWorkspaceInfo`, `WorkspaceRepoInfo`,
`WorkspaceDetail`, `RecentProjectEntry`). These are wire types
that don't have schema equivalents; they're intentionally in the
DTO module. No work needed; documenting for the next agent who
wonders why `dto.rs` isn't empty.

---

## CI guardrails still advisory for the migrated set

**Severity: low** (mechanical tightening)

After Phase 3.2 ships the federation/workspace followup, the
`KNOWN_DISPATCH_ARMS` set in `scripts/check-mcp-dispatch-shape.py`
collapses to empty. The `ARM_GROWTH_BUDGET = 3` should drop to
`0`. The sensor-check script's `KNOWN_LEGACY_SENSORS` set
collapses to empty after Phase 3.1 (it already has the trait
shape). The DTO check's `KNOWN_MIRRORS` collapses to empty after
3.3. None of these are bugs; they're guardrails that should
tighten as their corresponding refactor lands.

---

## CI guardrail: bincode `skip_serializing_if` on `Option`

**Severity: low** (one-time lesson; not a followup)

Discovered during Phase 3.3: bincode 2.x's positional encoding
breaks when `#[serde(skip_serializing_if = "Option::is_none")]`
elides fields on a per-instance basis. The fix is
`#[serde(default)]` only — JSON readers tolerate the explicit
`null`. Documented in `src/server/schema.rs:381-386` next to the
`repo_id` field. New agents adding `Option` fields to types
that go through bincode should know this.

---

## Status legend

| Status | Count |
|---|---|
| Done in this work (PRs 6a–6f + docs/guardrails) | 9 items |
| Tracked here as followup | 12 items |
| Pre-existing, not introduced | 1 item (`feat_negative_paths.rs:218`) |
