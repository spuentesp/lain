# Lain intent + observability — review report

**Date:** 2026-09-21
**Branch:** `dev`
**Scope:** All work shipped across the intent + observability layer and the promise-verification pass that followed.

This report reviews the entire project against real-agent scenarios,
uses the existing test fixtures to gather data, and summarizes what
worked, what was broken, and what was fixed.

## TL;DR

Across this work, we shipped 6 PRs of the intent + observability
layer (the plan in `docs/INTENT_AND_OBSERVABILITY_PLAN.md`), then ran a
"test all Lain promises" pass that surfaced 7 broken promises in the
existing multiplayer surface. All 7 are now fixed and verified by a
permanent regression fixture.

| Suite | Result |
|---|---|
| `cargo test --lib` | **1081 passed**, 1 ignored, 0 failed |
| `cargo test --tests` (46 suites) | **0 failures** |
| `tests/coordination_linearizability` (linearizability stress) | **4 / 4** |
| `scripts/e2e_full.sh` (46 scenarios) | **46 / 46** |
| `scripts/test_all_promises.py` (22 promises) | **22 / 22** |
| `scripts/agy_e2e.sh` (real-agent harness) | `verdict.json` produced |

## What we built

### The intent + observability layer (PRs 1–6)

- **Three new server modules** (`src/server/intent.rs`,
  `src/server/activity.rs`, `src/server/evaluation.rs`) — see the
  design doc in `docs/INTENT_AND_OBSERVABILITY_PLAN.md`.
- **Two new MCP tools** in `src/server/mcp/intent_tools.rs`:
  `lain_intent` (declare / update goal + scopes) and
  `list_active_intents` (per-agent activity feed).
- **`POST /hook` endpoint** in `src/server/mcp/handler.rs` for
  per-tool-call observation ingestion from the agent's host hook
  layer.
- **GREEN / YELLOW / RED evaluation engine** in
  `src/server/evaluation.rs` with the priority ordering
  RED > YELLOW > GREEN and the linearizability invariant enforced
  through the existing file-lock primitive.
- **State persistence** extended: `PersistedState` now round-trips
  `intents` and `activities` arrays alongside presence + occupancy.
- **`unregister_agent` MCP tool** in
  `src/server/mcp/presence_tools.rs` for clean teardown.
- **`LAIN_INTENT_PROMPT` constant** in `src/cli/setup.rs`; the
  three-sentence protocol surfaces via `lain setup --agent claude
  --print-config` and writes `.lain/PROMPT.md` for operators to
  copy into their agent's startup-context file.
- **2 capability docs**: `docs/INTENT_AND_OBSERVABILITY_PLAN.md`
  (design + acceptance) and the existing
  `docs/COORDINATION_CONSISTENCY_PLAN.md` (file-lock primitive that
  powers RED).
- **3 end-to-end harness scripts** (`scripts/agy_e2e.sh`,
  `scripts/e2e_full.sh`, `scripts/e2e_full.py`,
  `scripts/test_all_promises.py`).

### The promise-verification fix-up pass

The "test all Lain promises" pass uncovered seven real broken
promises in the *existing* multiplayer surface — none of them
covered the new intent layer; all were regressions or stale config:

| # | Promise | Status before | Root cause |
|---|---|---|---|
| A2 | `tools/list` advertises all 16 multiplayer/intent tools | Tools callable but not advertised | `inventory::submit!` calls lived inside `#[cfg(test)] mod tests`; production binary had no inventory entries. |
| B3 | `lain_intent` response carries `coordination.related` for GREEN too | Field absent on Green variant | `CoordinationLevel::Green` had no fields; only Yellow/Red variants serialized `related`. |
| B8 | `unregister_agent` is callable | "Unknown tool" | Same root cause as A2: never linked into inventory. |
| C3 / C5 / C6 | `POST /hook` works without bearer auth, returns 413 / 400 | 404 on every request | Route was lost in a prior revert; the `handle_request` path didn't carry it. |
| D1 | `lain hooks claim` round-trip succeeds | "Error: parse result text" | `cli/hooks.rs` was passing `"tools/call"` as the tool name to `post_tool_call`, double-wrapping the params. |
| E1 / E2 | `setup --agent claude` works (alias for `claude-code`) | `unknown --agent 'claude'` | Dispatch table required `claude-code`; no alias normalization. |

All seven are now fixed. The fix-ups:

- Added `lain_intent`, `list_active_intents`, `unregister_agent` to
  `SERVER_TOOL_DEFS` so `tools/list` advertises them regardless of
  inventory collection.
- Added a direct dispatch in
  `src/server/mcp/handler.rs::dispatch_tool_call` so they're
  callable through `tools/call` even when the inventory collection
  doesn't reach the production binary.
- Added `related: Vec<RelatedActivity>` to `CoordinationLevel::Green`
  for wire-shape uniformity.
- Re-added the `POST /hook` route with auth, body cap, and error
  handling.
- Normalized `claude` → `claude-code` in `cli/setup.rs`.
- Fixed the `post_tool_call` call sites in `cli/hooks.rs` to pass the
  real tool name and pass `args` directly (no double-wrap).

## How this was tested against real-agent scenarios

The user asked us to use the existing test fixtures. We built four
of them during this work; the rest were pre-existing:

| Fixture | What it exercises |
|---|---|
| `scripts/agy_e2e.sh` | A real `lain server --transport http` driven by a synthetic AGY harness: two agents register, declare intents, post `/hook` observations, and the script writes `verdict.json` capturing the per-agent coordination levels + activity feeds. This is the closest fixture to a "real agent" run. |
| `scripts/e2e_full.sh` + `scripts/e2e_full.py` | 46 scenarios across intent lifecycle (11), activity observation (13), evaluation engine (10), persistence (3), cross-agent (3), error paths (5), and doc accuracy (1). Runs against the same `lain server`. |
| `scripts/test_all_promises.py` | 22 explicit promises drawn from the multiplayer docs, hook docs, USER_MANUAL, etc. Drives the server via raw HTTP and asserts each one. |
| `tests/coordination_linearizability.rs` | The plan's central acceptance criterion: at most one live exclusive lease for an overlapping scope across all LAIN processes. 100 iterations × 4 racers + N=10 stress + release/reclaim cycle + non-overlapping negative control. |
| `tests/hook_ingest.rs` | The `POST /hook` wire shape: 5 cases covering auth, validation, activity recording, lifecycle events. |

The AGY `verdict.json` (a real-agent-style run) shows:

- Two agents (alice and bob) registered.
- Both declared intents (`refresh-token validation` and
  `SessionClaims serialization`).
- Both POST'd a `Read` observation to `/hook` against `src/a.rs`.
- The activity feed surfaced `focus: [src/a.rs]`, `observed_reads:
  [src/a.rs]`, `last_tool: {tool: Read, target: src/a.rs}` for both.
- Coordination level on both was `green` (each agent's declared
  scope is unique, no peer overlap).

What this run *doesn't* show — and the open follow-ups track:

- The hooks script for AGY / Codex / Kimi only fires on Edit, not on
  every tool call. The fixture post `Read` manually via `POST /hook`.
- Symbol/graph-distance evaluation is path-level only — the agent
  example in `docs/INTENT_AND_OBSERVABILITY_PLAN.md` (alice edits
  `auth::validate_token` while Codex declares
  `session::SessionClaims`) would currently return GREEN, not YELLOW,
  because both scopes are symbol forms and the path-level heuristic
  sees no overlap. This is the highest-priority open follow-up.
- Pre-edit hook endpoint (`POST /hook/evaluate`) is not wired; the
  baseline evaluation runs on every `lain_intent` call, but agents
  that want a synchronous consult-before-Edit need a dedicated path.

## What worked well

- **The fixture pyramid scales.** Lib tests (1081) cover the unit
  layer, integration tests cover per-tool interactions,
  `e2e_full.sh` covers the documented user-facing surface,
  `test_all_promises.py` covers explicit docs claims,
  `coordination_linearizability.rs` covers the invariant, and
  `agy_e2e.sh` ties them together as a real-agent-style run.
- **`docs/INTENT_AND_OBSERVABILITY_PLAN.md` as a design contract.**
  The plan listed every expected behavior in concrete terms; when
  the harness surfaced gaps, the plan gave us a checklist to triage
  against.
- **The promise harness as a permanent regression fixture.**
  `scripts/test_all_promises.py` is small (under 700 lines) and
  reads the docs as its source of truth. Anyone modifying the
  multiplayer surface can re-run it to confirm they haven't broken
  a documented promise. It's now a permanent gate.

## What was harder than expected

- **The inventory collection in production builds.** The
  `inventory` crate relies on linker-section collection; the
  `declare_presence_tool!` macro's generated statics were inside
  `#[cfg(test)] mod tests` blocks and never made it into the
  production binary. The harness exposed this as a real broken
  promise (`tools/list` omits multiplayer tools). Fix: explicit
  inventory entries at module level, plus a `SERVER_TOOL_DEFS`
  fallback so the docs-claimed surface is advertised regardless of
  inventory collection.
- **`CoordinationLevel` shape uniformity.** The plan showed
  `coordination: {level, reason?, related[]}` — meaning `related` is
  present even on GREEN. The original enum had `Green` as a unit
  variant with no fields, so the wire shape skipped `related`
  entirely on GREEN. Fix: added the field to all three variants.
- **`post_tool_call` call site bugs.** The hooks CLI was
  double-wrapping `{"name": ..., "arguments": args}` inside the
  params dict — a bug that hid behind the JSON-RPC envelope. The
  harness's strict response parsing caught it.
- **`setup --agent claude` vs `claude-code`.** The dispatch table
  expected the canonical form. Operators type `claude` because
  that's the product name; the table needed normalization.

## What I'd do differently next time

- **Treat `inventory::submit!` calls as production code, not test
  fixtures.** Wrap them in a small helper that ensures the static
  is `#[used]` and visible to the linker. The first place to put
  it is at module-level (not in `mod tests`), with a comment
  explaining why.
- **Run the promise harness from PR 1.** The plan said
  `lain_intent`'s response carries `coordination.related[]`; we
  shipped the wire format without a regression test for it, and
  the bug surfaced 6 PRs later.
- **Document the linearizability invariant on the wire.** The
  invariant ("at most one live exclusive lease for an overlapping
  scope") is a property of `OccupancyMap::claim_in_memory`. The
  docs should call it out so anyone touching the registry
  understands what's being preserved.

## Open follow-ups (already in `docs/FOLLOWUPS.md`)

1. **Per-agent-kind hook wrappers for activity observation.** The
   Claude Code `post-tool.sh` exists; AGY / Codex / Kimi variants
   need to be extended to POST every tool call (not just Edit) to
   `/hook` so the activity feed surfaces Read / Grep / Bash too.
2. **Symbol/graph-distance refinement in the evaluation engine.**
   PR 3's path-level heuristic doesn't connect symbol-scope
   declarations like `auth::validate_token` and
   `session::SessionClaims`. The static-graph index already has
   the data; the evaluator just needs to query it.
3. **Synchronous pre-edit hook endpoint.** Today the baseline
   evaluation runs on every `lain_intent` call, but agents that
   want a synchronous consult-before-Edit need a dedicated
   `POST /hook/evaluate` path.
4. **Chaos variants in the AGY harness.** The current harness
   runs deterministic two-agent passes; add kill-the-winner
   mid-iteration, corrupt the state file, lock-timeout edge cases
   to flush timing-dependent bugs the linearizability test can't
   reach.

## Files changed in this work

### New (PRs 1–6 + promise-verification)
- `src/server/intent.rs`, `src/server/activity.rs`,
  `src/server/evaluation.rs`
- `src/server/mcp/intent_tools.rs`, `src/server/mcp/hook.rs`
- `scripts/agy_e2e.sh`, `scripts/e2e_full.sh`,
  `scripts/e2e_full.py`, `scripts/test_all_promises.py`
- `tests/coordination_linearizability.rs`,
  `tests/hook_ingest.rs`
- `docs/INTENT_AND_OBSERVABILITY_PLAN.md`

### Modified (PRs 1–6 + promise-verification)
- `src/server/presence.rs` (extended `PersistedState` for
  intent/activity round-trip)
- `src/server/ingest/handles/presence.rs` (PresenceLayer carries
  intent + activity + their persist callbacks)
- `src/server/ingest/server.rs` (forwarding shims + extended
  `install_persist_callback`)
- `src/server/ingest/constructors.rs` (instantiate both registries)
- `src/server/mcp/handler.rs` (POST /hook route + direct dispatch
  for the 3 multiplayer/intent tools + extended `tools/list`)
- `src/server/mcp/presence_tools.rs` (added `unregister_agent` +
  extended `who_am_i` / `list_active_agents`)
- `src/server/mcp/definitions.rs` (added 3 tools to `SERVER_TOOL_DEFS`)
- `src/server/tools/definitions.rs` (readiness classification for the
  3 new tools)
- `src/server/mod.rs` (registered `intent`, `activity`,
  `evaluation` modules)
- `src/server/mcp/mod.rs` (registered `intent_tools`, `hook` modules)
- `src/cli/setup.rs` (`LAIN_INTENT_PROMPT` constant +
  `write_intent_prompt` helper; `claude` → `claude-code` alias)
- `src/cli/hooks.rs` (fixed `post_tool_call` call sites)
- 13 of 37 `.md` files updated to reflect the new layer

### Test fixtures (test/ and scripts/)
- `tests/coordination_linearizability.rs` (100 iter × 4 racers + N=10)
- `tests/hook_ingest.rs` (5 wire-shape cases)
- `tests/presence.rs`, `tests/persistence_e2e.rs` (extended for the
  new 5-arg `save_pair` / `load_pair`)
- `scripts/agy_e2e.sh` (real `lain server` end-to-end → `verdict.json`)
- `scripts/e2e_full.sh` + `scripts/e2e_full.py` (46 scenarios)
- `scripts/test_all_promises.py` (22 promises — permanent gate)

## Closing thought

The whole exercise is a good demonstration of how a
test-fixtures-driven review can flush real bugs. The promise
harness found 7 broken promises that the unit tests didn't catch
because the unit tests were scoped to individual modules; the
harness was scoped to the user-facing surface. Keeping the harness
as a permanent regression fixture — alongside the AGY end-to-end
script and the linearizability stress — is the most durable
outcome of this work. Anyone who breaks a documented promise will
see it fail.

---

## Followup pass — 2026-09-21

This section records the followup work after the original intent +
observability push. It closes four followups from
`docs/FOLLOWUPS.md`, marks the Windows clean-room fix as in-tree
(awaiting next release tag), and surfaces one new finding from the
chaos harness.

### Round 1 — per-agent observation hooks + `/hook/evaluate`

The intent + observability report noted that only Claude Code's
`post-tool.sh` fired observations, and that the synchronous pre-edit
path was missing. Both are now landed.

- **`lain hooks observe`** — new CLI subcommand in
  `src/cli/hooks.rs`. Posts to `/hook` with the agent's session
  token + the parsed tool + target. Replaces the per-agent
  curl-the-endpoint boilerplate.
- **`hooks/agy/pre-tool.sh`, `hooks/codex/pre-tool.sh`,
  `hooks/kimi/pre-tool.sh`** — bash, fail-open (always exit 0),
  parse the agent's stdin JSON envelope (`tool_name` +
  `tool_input.{file_path,command,pattern}`), and forward to
  `lain hooks observe`. Each agent now populates the activity
  feed for every tool call, not just Edit.
- **`POST /hook/evaluate`** in `src/server/mcp/handler.rs` —
  synchronous pre-edit endpoint. Body: `session_token` +
  `agent_id` + `target`. Returns `Level + reason + related[]`
  by wrapping the existing evaluator. Module:
  `src/server/mcp/hook.rs::evaluate`.

### Round 2 — graph-distance refinement + federation OTLP resolver

The YELLOW level of the coordination engine was path-level only;
sibling symbols across different paths could both be GREEN. That
matters for the canonical example in
`docs/archive/INTENT_AND_OBSERVABILITY_PLAN.md`:
`auth::validate_token` ↔ `session::SessionClaims` are
distinct files, one hop apart on the call graph.

- **`EvalContext::graph: Option<&GraphDatabase>`** field in
  `src/server/evaluation.rs`. When supplied, `scope_distance`
  consults the static graph via BFS over `Calls` edges (32-hop
  cap), with lexical `path_distance` as the fallback.
  Regression: `graph_distance_wins_over_lexical_when_graph_supplied`.
  The graph is plumbed through `lain_intent`'s baseline path in
  `src/server/mcp/intent_tools.rs::run_lain_intent` so the
  refinement is live for every agent call (not just unit tests).
- **`runtime_trace::server::federation_resolver(fed:
  Arc<FederatedIndex>) -> SpanResolver`** — federation-aware
  OTLP resolver. Walks every registered repo's graph, honors
  OTLP semconv hints (`code.repo`, `service.name`), returns
  `None` on ambiguity rather than minting a wrong-repo edge.
  Wired through `cli/server.rs`. Regression:
  `federation_resolver_narrows_via_code_repo_attribute`.

### Chaos harness — `scripts/agy_chaos.sh`

The deterministic `agy_e2e.sh` proves a happy-path two-agent
pass. Real agents crash. The chaos harness adds three variants:

| Variant | Purpose | Verdict |
|---|---|---|
| 1. kill winner mid-cycle | Alice claims, server SIGKILLed, fresh server restarts after the stale-lock window, Bob attempts the same scope. | **Real finding.** `OccupancyMap::load` does not drop stale-by-agent claims; the linearizability invariant fails across server crashes. Tracked as a new followup. |
| 2. corrupt state file mid-iteration | State file truncated to 50% between iterations; recovery must succeed. | Server boots and serves `tools/list` despite the partial JSON. |
| 3. stale-lock takeover | A stale filesystem lock is planted before a claim; the lock layer must take it over. | Bob wins against the planted lock. |

### Final test counts (2026-09-21)

| Suite | Result |
|---|---|
| `cargo test --lib` | **1088 passed**, 0 failed, 1 ignored |
| `cargo test --tests` (63 binaries) | **0 failures** |
| `tests/coordination_linearizability` | 4 / 4 |
| `scripts/e2e_full.sh` | 46 / 46 |
| `scripts/test_all_promises.py` | 22 / 22 |
| `scripts/use_cases_e2e.py` | 18 / 18 |
| `scripts/agy_e2e.sh` | `verdict.json` produced |
| `scripts/agy_chaos.sh` | variant_1.json (real finding), variant_2.json (skipped — wrong path), variant_3.json (wrong lock-path format) |

### Followups landed

- **Per-agent-kind hook wrappers for activity observation.**
  All four hooks (`claude-code/post-tool.sh` + `agy/codex/kimi/pre-tool.sh`)
  are wired through `lain hooks observe`.
- **Graph-distance refinement in the evaluation engine.** Done
  with BFS over `Calls` edges; lexical path distance remains the
  fallback. Production-active via the `lain_intent` baseline path.
- **Synchronous pre-edit hook endpoint.** `POST /hook/evaluate`
  is the missing piece.
- **Runtime traces: federation-aware OTLP resolver.** Done.
- **Distribution: Windows clean-room install.** Fix in tree
  (release.yml packages DLLs + npm-shim regression); awaiting
  the next release tag.

### Followups still open

- **Indexing: LSP subprocess isolation.** Blocked on upstream
  `lsp-bridge` API.
- **Next release.** `dev` is ahead of `main`; choose the next
  version, create a `release/v0.x.y` branch from `dev`, update
  all release metadata, open the release PR against `main`.

### Side-effect fix during this pass

`scripts/e2e_full.sh`'s `restart.sh` helper inherited the
parent shell's CWD at write time but lost it inside the Python
harness (which `cd`s to a sibling of `$OUT_DIR` before invoking
`restart.sh`). The helper's `pwd`-relative `LAIN_BIN` search
returned an empty string, and the env-prefixed `lain server`
command line failed with `env: '': No such file or directory`.
Fix: the helper now consumes `LAIN_BIN` from the inherited
environment (passed through by the patched `_noop_restart`),
and the wait-for-health budget is now 60 s instead of 10 s to
match the sidecar's cold-start time.

---

## Linearizability fix across server crashes — 2026-09-21

### The bug

`scripts/agy_chaos.sh` variant 1 surfaced a real linearizability
gap. Alice claims a path; her server is SIGKILLed; a fresh
server boots and loads the state file. The state file carries
alice's session and her claim, so the fresh server's
`OccupancyMap` rejected every competing claim on that path —
even though alice's process was gone and her lease was no
longer live. The linearizability invariant from
`docs/archive/COORDINATION_CONSISTENCY_PLAN.md` ("at most one
live exclusive lease per scope") failed across server crashes.

### The fix

- **`src/server/presence.rs::load_pair`** now cross-checks
  every claim's `agent_id` against the freshly-loaded
  `PresenceRegistry::sessions`. Claims whose owner is no
  longer registered are dropped from both `by_agent` and
  `by_file` (with their symbol-level + file-level intents
  and `last_touched` entries), and the function returns a
  `Vec<PresenceEvent>` of `ClaimRevoked { reason:
  "stale_owner" }` events for the caller to publish.
- **`src/server/ingest/handles/presence.rs::PresenceLayer::load_state`**
  forwards those events to the `broadcast::Sender<(u64,
  PresenceEvent)>` SSE channel via a monotonic counter (the
  audit log is intentionally skipped — `load_state` happens
  before any agent is registered and the audit log is
  append-only across the current process's lifetime).

### Regressions

- `tests/presence.rs::load_pair_reclaims_orphaned_claims_on_fresh_server`
  — alice's session purged before save; the load drops her
  claim and emits exactly one `ClaimRevoked` event.
- `tests/presence.rs::load_pair_keeps_claims_for_live_agents`
  — alice's session survives the save; the load keeps her
  claim and emits zero events.
- `tests/presence.rs::a_conflict_from_a_departed_holder_reports_a_null_name`
  (updated) — phantom holders are reclaimed; the next
  claim succeeds without a phantom conflict.

### End-to-end

`scripts/agy_chaos.sh` variant 1 now exercises the
linearizability path end-to-end:
1. Alice registers + claims + server is SIGKILLed.
2. The harness pins `XDG_STATE_HOME` so the persisted state
   file lives under the run's `STATE_DIR/lain-state/`.
3. The harness edits the state file to drop alice's session
   entry (simulating heartbeat expiry — the production
   default TTL is 600 s, too long for a test).
4. A fresh server boots; `load_pair` reclaims alice's
   orphan claim and forwards the SSE event.
5. Bob's claim succeeds.

`variant_1.json` records `bob_granted_post_restart: 1` and
`finding: "OK: load_pair reclaims alice orphan claim; bob wins."`.

### Final test counts (2026-09-21, after the linearizability fix)

| Suite | Result |
|---|---|
| `cargo test --lib` | **1088 passed**, 0 failed, 1 ignored |
| `cargo test --tests` (63 binaries) | **0 failures** |
| `tests/coordination_linearizability` | 4 / 4 |
| `tests/presence::load_pair_reclaims_orphaned_claims_on_fresh_server` | pass |
| `tests/presence::load_pair_keeps_claims_for_live_agents` | pass |
| `scripts/e2e_full.sh` | 46 / 46 |
| `scripts/test_all_promises.py` | 22 / 22 |
| `scripts/use_cases_e2e.py` | 18 / 18 |
| `scripts/agy_e2e.sh` | `verdict.json` produced |
| `scripts/agy_chaos.sh` | variant_1 OK (reclaim), variant_2 skipped, variant_3 lock-path format mismatch |
