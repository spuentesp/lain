# Coordination: Staleness Detection, Audit Trail, Dashboard Noise

## Goal

Close three gaps in Lain's multiplayer coordination that surfaced from external
feedback and that remain open after the rounds of work already shipped:

1. **Staleness window** (dataciv). An agent that queries `query_graph` or
   `get_blast_radius`, plans, and then claims has no way to know whether the
   world changed in between. The agent's plan may be built on a stale snapshot.
2. **Audit trail for who raced with whom** (lobsternigel). After an edit lands,
   the system records that the claim existed but not who else held overlapping
   claims at the moment the edit fired, and what revisions bracketed the
   operation.
3. **Dashboard noise** (hermes_cli). During broad refactors with multiple
   agents active, advisory `conflict_detected` events flood the SSE stream and
   the dashboard, training agents to ignore conflicts in general — including the
   one real collision that mattered.

The design treats these as a coherent unit because the same primitives
(overlay revision, ring buffer of diffs, persistent audit) serve all three.

## Non-goals

- **Identity / trust provenance** (concordiumagent's point). Token signing and
  an allow-list of `AgentKind` are out of scope. They are orthogonal to
  coordination and live in a separate auth layer.
- **The stderr audit log in `src/server/attribution.rs:380`.** That
  `eprintln!("[attribution] unattributed edit: …")` covers an
  attribution-failed case and stays as-is. The structured `audit.jsonl`
  introduced here covers a different case (coordination race at write
  time, populated only when the agent claim flow ran successfully).
  Both coexist; this spec does not touch `attribution.rs`.
- **Cross-server federation revision**. Each `lain server` process keeps its
  own revision; the design does not attempt to coordinate stale state across
  instances.
- **Static-graph convergence fixes** (already in flight on branch
  `fix/index-convergence-canonical-paths`: `ae06a9a`, `4bde9a8`, `1f3211f`,
  `4cbf654`, `8014581`). When those land, retraction events become visible
  through normal graph responses; this spec covers the *delta-detection*
  surface on top of them.

## Background — what's already shipped

Read this section before re-implementing anything; the design reuses these
primitives.

- **`RevisionId = u64`** exists as a per-process monotonic counter in
  `src/server/overlay/stream.rs:18`. Every `OverlayDiff` carries
  `revision: RevisionId` (`stream.rs:25`). What does not exist is exposing
  this counter on tool responses or letting `claim_files` refer back to it.
- **Staleness fix (4-step series).** Step 1 landed in commit `008aa3b`:
  `RefreshOutcome` persisted on `LainServer`, configurable
  `--reindex-timeout`, banner visible via `get_health` Markdown line. Steps
  2-4 (per-file freshness, sync re-parse) are separate plans. **This is
  server-side graph freshness; the staleness window in this spec is
  agent-side — different problem.**
- **Scoped freshness notes at query time** landed in commit `432e883`:
  `GraphDatabase::freshness(workspace, path) -> Freshness{Fresh, Dirty,
  Absent}` returns a one-line note ("⚠ src/foo.rs was modified 4m ago")
  on responses from `explain_symbol`, `semantic_search`, `find_dead_code`,
  `query_graph`, `metrics`. **This is *query-time* staleness signaling at
  the file level (mtime-vs-last-scan).** The mechanisms introduced here
  sit *orthogonally* on the response side: `revision: u64` (overlay
  monotonic counter on every response), `world_state` (claim-time
  per-symbol delta), and the existing `Freshness::note` (per-file
  freshness at query time). Three layered signals on different timescales.
  This spec does not modify the freshness mechanism.
- **Persistence** of `PresenceRegistry` + `OccupancyMap` is complete
  (`7790b3f` + `356ba9c`); `started_at` and `last_heartbeat` survive restarts.
- **`OccupancyMap::claim` read-vs-edit filter** (`a00353d`) and conflict
  entries carrying `intent` + `last_touched_unix` are shipped. The wishlist
  item #5 ("advisory conflicts should say *what*, not just *that*") is closed
  on that surface.
- **`detect_overlap` MCP tool** (commit-time overlap) is shipped with
  graduated severity weighting `none`/`low`/`medium`/`high`
  (`b05ae78`). This is a different surface from runtime
  `conflict_detected` events; the severity work in this spec touches the
  runtime side.
- **Filesystem-as-lock layer** is shipped (`a250962`). Authoritative state
  remains in memory; the filesystem is a best-effort hint.
- **Index convergence** for the static graph: retractions of deleted symbols
  are now correctly propagated through `project_repo` (commits on branch
  `fix/index-convergence-canonical-paths`). Stale symbols no longer answer
  queries, but retractions do not yet bump the overlay's revision.

## Architecture

Throughout this spec, `RevisionId` refers to the existing
`pub type RevisionId = u64;` in `src/server/overlay/stream.rs:18` — a
per-process monotonic counter emitted on every `OverlayDiff`. We do not
introduce a new revision type.

### `RevisionId` surface lift

Add to `src/server/overlay.rs`:

- Public method `VolatileOverlay::current_revision() -> u64` returning
  the most recent revision emitted by `insert_node` / `insert_edge`.
- A bounded ring buffer `RevisionLog` (new struct, owned by
  `VolatileOverlay`) holding the last 256 `OverlayDiff` events keyed by
  revision. Wrap-around is LRU — older diffs are evicted; queries past the
  floor receive `LookupResult::TooOld`.
- `VolatileOverlay::diffs_since(revision: u64) -> Result<Vec<OverlayDiff>,
  LookupResult>` with `LookupResult::{Ok, BeyondCurrent, TooOld}`.

Each tool response carries `revision: u64` in the **outer envelope**
(i.e. on the `CallToolResult` returned to MCP, alongside `is_error`),
NOT inside the tool's `content[0].text` JSON. The MCP SDK in use is
`rmcp`; if its `CallToolResult` exposes a `_meta` / `meta` field for
arbitrary metadata, that's where `revision` lives. Otherwise the field
is set at the JSON-RPC `result._meta` level. Either way it is sibling
metadata of the tool payload, not a field embedded in the payload.

This is deliberately additive: the inner tool payload shape is
preserved exactly as it was. Bare arrays stay bare arrays (e.g.
`list_repos` continues to return `Vec<RepoInfo>` as a top-level JSON
array), primitive payloads stay primitive, Markdown tool payloads like
`get_health` and `get_agent_strategy` remain Markdown strings. No
existing consumer of any tool response (e2e shell scripts in
`tests/e2e/`, the Command Center SPA in `src/ui/`, hooks under
`hooks/{claude-code,kimi,agy,codex}/`, or external callers) needs to
change to accommodate `revision`.

The construction site is unified: every `CallToolResult` constructed
by `handle_call_tool_request` (stdio) and the `tools/call` JSON-RPC arm
of `handle_request` (HTTP) — including error sites, presence-tool
dispatches, executor-fallback paths, and Markdown tools — flows
through one envelope constructor that reads `overlay.current_revision()`
and attaches the metadata. Streaming carve-outs (`/events` SSE,
`/overlay/subscribe` ndjson) do not return `CallToolResult` and are not
touched.

### `Claim` extension

Add one field to `Claim` (`src/server/presence.rs:144-173`):

```rust
pub plan_revision: Option<RevisionId>,
```

Populated when `claim_files` request includes `plan_revision`. Persisted via
`#[serde(default)]` so existing state files load without migration.

The earlier draft of this spec also proposed `claimed_at_revision`. After
review, that field is dropped. Audit log entries carry both `plan_revision`
and `landed_revision`, so the audit is the canonical correlation record;
adding a parallel field on the Claim itself duplicated state and made the
audit-failure path ambiguous.

### `claim_files` request and response

`src/server/mcp/presence_tools.rs` — additive changes only.

Request gets one new optional field:

```rust
pub struct ClaimRequest {
    pub path: PathBuf,
    pub symbols: Vec<String>,
    pub intent: ClaimIntent,
    pub ttl_seconds: Option<u64>,
    pub plan_revision: Option<RevisionId>,
}
```

Response gets one new optional field:

```rust
pub struct ClaimResult {
    pub granted: Vec<ClaimRequest>,
    pub conflicts: Vec<ConflictEntry>,
    pub world_state: Option<WorldState>,
}

pub struct WorldState {
    pub current: RevisionId,
    pub plan: RevisionId,
    pub changed_symbols: Vec<ChangedSymbol>,
    /// Free-form note for the agent. Populated on `BeyondCurrent` and
    /// `TooOld` error paths so the agent knows whether to resync or to
    /// proceed under advisory. `None` on the success path.
    pub note: Option<String>,
}

pub enum ChangedKind {
    /// Symbol updated via overlay `OverlayDiff` since `plan`.
    Edited,
    /// Symbol removed from the static graph (retracted by
    /// `project_repo`). `at_revision` is the revision at the moment
    /// of claim detection, not the revision where the retract
    /// originally occurred.
    Retracted,
}

pub struct ChangedSymbol {
    pub name: String,
    pub change_kind: ChangedKind,
    pub at_revision: RevisionId,
}
```

`world_state` is `Some(_)` only when the request included a `plan_revision`;
`None` otherwise, so existing hooks that don't pass `plan_revision` see no
change in response shape. `changed_symbols` is deduplicated by `(name,
change_kind)` — multiple `OverlayDiff` events that touch the same symbol
collapse to one entry with the most recent `at_revision`.

Audit event written to `<state_dir>/audit.jsonl` and emitted as the SSE
`edit_landed` payload:

```rust
pub struct AuditEvent {
    pub ts_unix: f64,
    pub agent_id: AgentId,
    pub path: PathBuf,
    pub claim_set: Vec<Claim>,
    pub racers: Vec<ConflictEntry>,
    pub plan_revision: Option<RevisionId>,
    pub landed_revision: RevisionId,
}
```

Seven fields — matches the contract referenced in the testing section.

The SSE wire shape for `edit_landed` uses serde's external-tag default:
the JSON `data` is `{"EditLanded": {"event": {<seven audit fields>}}}`.
Consumers that want the audit fields read `data["EditLanded"]["event"]`.
The SSE frame's `event:` field is set to `"edit_landed"`, so a header-only
subscriber can identify the variant without parsing the body.

> **Decision note (added during final review).** Task 2.4's plan originally
> specified a hand-rolled `Serialize` impl that flattened the audit
> fields to the JSON top level. Per-task reviews passed without running
> cargo (PATH didn't include rustup). The whole-plan final review
> caught the regression: the flatten dropped the variant tags from
> every `PresenceEvent` variant's wire JSON, breaking two pre-existing
> tests in `tests/presence.rs` and `src/server/sse.rs::tests`. The fix
> here is the simpler `#[derive(Serialize)]` form. The trade-off: edit_landed
> is now externally-tagged (`{"EditLanded": {"event": {...}}}`), so
> consumers that want the audit fields parse one level deeper than
> originally planned. The seven audit fields and their JSON keys are
> unchanged.

### Static-graph retract detection at claim time

When computing `world_state.changed_symbols`, scan the requested symbols
against the static graph in addition to the overlay diffs:

- **Retracted.** The symbol name resolves to no node in the active
  workspace's `symbol_to_repos` index (federation) or per-repo
  `GraphDatabase::find_nodes_by_name` (single-workspace). Add to
  `changed_symbols` with `change_kind: Retracted` and
  `at_revision: current_revision()` (the revision at the moment of
  claim detection — the time the retract happened is not tracked).
- **Edited.** The symbol name resolves in the static graph but the
  `SymbolHash` content hash (BLAKE3-256 over the symbol body's byte
  range, already computed at index time and stored on `Claim.content_hash`)
  differs from the agent's plan-implied baseline. Add to
  `changed_symbols` with `change_kind: Edited`.

This is the **Option C** resolution (chequeo al claim). It avoids widening
the overlay's contract to track retractions and avoids introducing a second
revision dimension. It also catches the concrete failure mode: an agent
that plans around a symbol which has just been retracted.

The retraction lookup uses the federation's symbol-to-repos index in
federation mode and the per-repo DB in single-workspace mode; if the
symbol resolves in either, no retraction is reported. If absent, the
retract is reported even if no overlay diff exists for it (the retract
goes through the static graph backend, not the overlay).

### `write_context` and audit log

New module `src/server/audit.rs`:

```rust
pub struct WriteContext {
    pub claim_snapshot: Vec<Claim>,
    pub concurrent_editors_at_write: Vec<AgentId>,
    pub as_of_revision: RevisionId,
}

pub fn append_edit_event(
    state_dir: &Path,
    event: &AuditEvent,
) -> io::Result<()>;
```

SSE channel gets one new event variant: `edit_landed` carrying the same
shape as `WriteContext` plus the audit line that was (or was not) written.

Audit log file `<state_dir>/audit.jsonl`, append-only JSONL. Cap at 50 MB;
on overflow, rename existing to `audit.jsonl.1` (overwrite previous).
Persist `audit_offset_bytes: u64` in the main state file so appends continue
from where they left off across restarts.

On state-file load: if `audit.jsonl` is missing or corrupt, reset offset
to 0 and log a WARN. Persist `audit_reset_at_unix: Option<u64>` so
`get_audit_log` can report the gap on demand.

New MCP tool `get_audit_log(since_unix: i64, path_glob: Option<String>)`
in `src/server/mcp/audit_tools.rs`.

### Dashboard changes

`src/ui/` — no Rust changes.

- Add `severity: "none"|"low"|"medium"|"high"` to `conflict_detected`
  events emitted from the SSE handler. Computed server-side by the same
  logic that `detect_overlap` already uses
  (`detect_overlap` severity is shipped; extend to the runtime event).
- Add a "Only my session" toggle to the Agents online panel; toggle filters
  `conflict_detected` events client-side.
- Client-side burst collapsing: 3+ events with the same `path` within 5s
  collapse into a single card with `count: N, first_ts, last_ts` and a
  "show all" action.

## Data flow

Edit lifecycle with the new pieces inserted:

```
hook pre-edit
  → query_graph / get_blast_radius / get_coupling_radar
        response envelope: { ..., revision: u64 }
  → agent computes plan; holds plan_revision
  → claim_files(plan_revision=X, files=[...])
        server:
            read current_revision()  → Y
            for each requested symbol:
                check static-graph presence; mark Retracted if absent
            look up diffs_since(X) → Vec<OverlayDiff>
            convert diffs to ChangedSymbol (Edited)
            response: {granted, conflicts,
                       world_state?: {current: Y, plan: X,
                                      changed_symbols: [...]}}
  → if changed_symbols empty → proceed
  → if non-empty → agent decides: re-query, or proceed under advisory
  → bash hook executes the edit
  → server post-edit handler:
        append WriteContext + audit event to <state_dir>/audit.jsonl
        emit SSE "edit_landed" with the same payload
  → postmortem: get_audit_log(since_unix, path_glob)
```

## Wire compatibility

- All new fields on `ClaimResult`, tool responses, and audit events are
  additive. Existing hooks (`hooks/claude-code/{pre,post}-edit.sh`,
  `hooks/kimi/pre-edit.sh`, etc.) parse only the fields they care about
  and will continue to work without changes.
- `plan_revision` on `ClaimRequest` is optional; missing means "no
  plan_revision to validate against," and `world_state` is `None`.
- Existing state files load without migration; `plan_revision` defaults to
  `None` via `#[serde(default)]`.

## State file format

Extend the existing `~/.local/lain/state/<config-stem>.json`:

```json
{
  "presence_registry_v1": { ... existing ... },
  "occupancy_map_v1": { ... existing ... },
  "audit_offset_bytes": 12345,
  "audit_reset_at_unix": null
}
```

`RevisionId` is **not persisted**. After a server restart,
`current_revision()` returns 0 and walks up from there. Agents that
survive across server restarts must re-query before claiming; the
`plan_revision > current` error path handles this.

## Error handling

| Condition | Behavior |
|-----------|----------|
| `audit.jsonl` write fails (disk full, perms) | SSE `edit_landed` still emitted; stderr WARN; claim remains valid. The invariant is "audit never blocks an edit." |
| `plan_revision > current_revision()` | `LookupResult::BeyondCurrent`. `world_state.changed_symbols = []`, plus a `note: "plan_revision beyond current — server may have restarted"`. Agent must re-query. |
| `plan_revision < ring_buffer.floor` | `LookupResult::TooOld`. `world_state.changed_symbols = []`, plus `note: "plan_revision too old for delta; resync required"`. Agent must re-query. |
| `audit.jsonl` missing or corrupt on load | Reset offset to 0, WARN to stderr, persist `audit_reset_at_unix`. `get_audit_log` reports the gap on demand. |
| Static-graph retraction detected at claim | Symbol added to `changed_symbols` with `change_kind: Retracted`. No further action; agent decides. |
| `u64::MAX` wrap on `current_revision` | Out of scope; unrealistic at ~10^18 inserts. If it ever occurs, server restart resets. Documented in PR 1. |
| Hook requests `plan_revision` for an unsupported tool call | Server tolerates; the field is only consulted in `claim_files`. |

## PR split

Three PRs, sequenced:

### PR 1 — Revision surface lift + world_state + retract detection

- New `RevisionLog` struct in `src/server/overlay.rs` (or extracted to
  `src/server/revision_log.rs` if it grows).
- `VolatileOverlay::current_revision` and `diffs_since`.
- Helper in `src/server/mcp/handler.rs` injecting `revision: u64` on all
  tool responses.
- `Claim.plan_revision: Option<RevisionId>` field.
- `ClaimRequest.plan_revision`, `ClaimResult.world_state`, `ChangedSymbol`,
  `ChangedKind` types.
- Static-graph retraction lookup at claim.
- Update `docs/multiplayer.md` to document the new `revision` field on
  tool responses, the `world_state` field on `claim_files`, and the
  BeyondCurrent / TooOld error paths.
- Tests: ring buffer + monotonicity + lookup-bounds unit; integration
  covering agent-A query → agent-B edit-or-retract → agent-A claim sees
  the delta.

### PR 2 — write_context + audit log + SSE edit_landed

- New `src/server/audit.rs` with `WriteContext`, `AuditEvent`, append
  function, rotation, offset persistence.
- SSE channel emits `edit_landed`.
- New MCP tool `get_audit_log`.
- State file extended with `audit_offset_bytes`,
  `audit_reset_at_unix`.
- Tests: append contract, rotation, offset persistence, I/O-failure
  path, integration end-to-end.

### PR 3 — Dashboard UX

- `severity` on `conflict_detected` events.
- "Only my session" toggle in Agents online panel.
- Burst collapsing logic.
- SPA snapshot tests.

## Testing per PR

Coverage bars above. Specifics:

### PR 1

- **Unit:** ring buffer enqueue/dequeue/wrap-around with deterministic
  seed; `diffs_since(X)` returns `Ok` for valid X, `BeyondCurrent` for X
  > current, `TooOld` for X < floor.
- **Unit:** `world_state.changed_symbols` filters to the claim's
  `paths` only.
- **Unit:** retract detection at claim — symbol marked `Retracted` when
  static graph lacks it; present-but-rebuilt symbol marked `Edited` with
  new id; absent-in-static and no-overlay-diff still triggers `Retracted`.
- **Property:** `current_revision()` strictly monotonically increases
  under random insert/retract sequences.
- **Integration:** agent-A queries blast radius at revision X; agent-B
  inserts via overlay → agent-A claims at plan_revision X sees
  `Edited`; agent-B retracts via static-graph path → agent-A claims at
  plan_revision X sees `Retracted`.

### PR 2

- **Unit:** append produces exactly one JSONL line with the seven
  contract fields populated; rotation fires at 50 MB, moves `audit.jsonl`
  to `audit.jsonl.1`; offset persists across `drop`+reload.
- **Unit:** `get_audit_log(since_unix, path_glob)` filters correctly on
  timestamp and glob; returns empty list when no entries match.
- **Integration:** end-to-end edit cycle emits one audit line with
  correct `plan_revision`/`landed_revision`; concurrent agents produce
  lines in non-deterministic order but all references are valid.
- **Integration:** read-only workspace — SSE `edit_landed` still emits,
  stderr has WARN, claim valid.

### PR 3

- Snapshot tests: SPA renders `conflict_detected` with `severity=high`
  inside a collapsed group; "Only my session" toggle filters events;
  3 events same `path` within 5s collapse to one card with `count: 3`.

## Acknowledged gaps and follow-ups

- **Cross-server federation**. Each process keeps its own `RevisionId`.
  Coordinating revisions across instances is a separate concern.
- **Project_repo retract with no overlay revision bump**. This spec
  handles the read-side (claim-time detection) but does not push the
  retraction event through the overlay. A future PR may add an
  `OverlayDiff::Retract` variant and route `project_repo` through it;
  until then, retractions are caught at claim time but not during passive
  query responses.
- **Audit log retention beyond 50 MB × 2**. Two files of historical audit
  (current + one rotated) is the cap. Long-running sessions may want to
  ship to external log storage; out of scope here.
- **Static-graph retraction timeline**. We report retraction as a
  present/absent fact at claim time, not as a range of revisions. A
  future `static_gen` counter (rejected here as Option B for the retract
  gap) would give a temporal view if needed.
- **Identity / trust provenance** (concordiumagent). Out of scope;
  covered by separate auth work.

## Risks

- **Ring buffer cap = 256.** Generous enough for typical edit cadences
  but a long planning phase (hours) could exceed it. Mitigated by
  `TooOld` return plus the `note` field; the agent resyncs.
- **Audit log storage growth.** 50 MB cap mitigates; long-running
  production deployments may want a configurable cap or external sink.
- **`revision` field on tool responses.** Backward compatible but adds
  a field to all responses. If a caller serializes the response with a
  schema validator that expects an exhaustive set of keys, the new field
  could cause validation to fail. Documented in PR 1's `docs/multiplayer.md`
  update.
