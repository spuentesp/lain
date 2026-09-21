# Intent & observability plan

**Date:** 2026-09-20
**Status:** design, supersedes the file-lock work in `docs/COORDINATION_CONSISTENCY_PLAN.md` for the user-facing surface; the lock primitive from that plan survives as the RED-path enforcement
**Branch:** `dev`

## Problem statement

The current multiplayer surface exposes file paths and symbols as the
coordination primitive: an agent calls `claim_files("src/auth.rs")` and
either gets a grant or a conflict. That forces the agent to manually
orchestrate every coordination step and exposes only a narrow view of
what other agents are doing — a list of file paths is not a picture of
their work.

Two consequences the user-facing surface should fix:

1. **Agents don't share mental models.** "I'm refactoring authentication
   so refresh tokens can share validation" is the kind of intent a
   second agent would want to know about. The current surface reduces
   it to a list of file paths, which forces every coordination
   decision to happen at edit time instead of during planning.

2. **The fail-open coordination primitive is hidden behind a binary
   answer.** When `claim_files` returns "granted", the caller doesn't
   know whether that answer came from an authoritative lock or from a
   process that couldn't acquire the lock and proceeded anyway. The
   pre-edit decision needs three levels — green / yellow / red — so
   the agent can choose to proceed under caution rather than have the
   tool silently downgrade its guarantee.

## Goals

1. **Hooks automatically observe agent activity** (tool calls, file
   reads, commands, diffs). No agent code change required for the
   observation path.
2. **`lain_intent` gives the agent a tiny explicit API for declaring
   goal + scopes + status.** Three calls covers most sessions:
   declare, update, finish.
3. **Pre-edit evaluation** returns one of three levels:
   - **GREEN** — declared scope matches, no peer intent overlaps, no
     live exclusive claim on the target.
   - **YELLOW** — proceed with caution; reason surfaced (outside
     declared scope, peer intent with graph distance ≤ 2, peer is
     actively reading the same file).
   - **RED** — another agent holds an exclusive lease (claim) on this
     path/symbol. The agent must not proceed without releasing or
     expiring that lease.
4. **Cross-agent activity model** exposes per-agent `goal`, `status`,
   `focus`, `planned_changes`, `observed_reads` so a second agent can
   answer "what is the other agent doing?" without re-deriving it from
   file paths.
5. **System-prompt snippet** installed by `lain setup --agent claude`
   makes the protocol trivial: three sentences the agent sees when it
   starts.

## Non-goals

- Cross-machine distributed coordination. Same-machine only, same
  scope as the prior coordination plan.
- Inferring intent from the raw user prompt. The hook observes the
  prompt but does not treat it as authoritative intent — the agent
  still has to call `lain_intent`.
- Replacing the claim primitive. The file-lock fail-closed work from
  `docs/COORDINATION_CONSISTENCY_PLAN.md` is the RED-path enforcement;
  the new intent layer sits above it.
- Replacing the existing `PresenceRegistry` / `OccupancyMap`. Those
  remain the source of truth for *who is connected* and *who holds a
  claim*. The intent layer adds a third dimension: *what is each
  agent trying to do*.

## Architecture

```
                ┌───────────────────────────────────────────────┐
                │            agent (Claude Code, etc.)         │
                │                                               │
                │  lain_intent(goal, scopes, status)            │
                │  ─────────────────────┐                       │
                │                       ▼                       │
                │  Read / Grep / Bash   hooks observe           │
                │  ─────────────────────┐                       │
                │                       ▼                       │
                │  Edit                 pre-hook consults LAIN  │
                │  ─────────────────────┐                       │
                │                       ▼                       │
                │                       │ GREEN/YELLOW/RED     │
                │                       ▼                       │
                │  write patch          execute (or refuse)    │
                │  ─────────────────────┐                       │
                │                       ▼                       │
                │                       │ post-hook records    │
                │                       ▼                       │
                │  done                 stop / subagent-stop   │
                └───────────────────────┬───────────────────────┘
                                        │ hook events
                                        ▼
                ┌───────────────────────────────────────────────┐
                │                  lain server                   │
                │                                               │
                │  ┌──────────────┐    ┌─────────────────────┐  │
                │  │ IntentRegistry│    │ ActivityTracker     │  │
                │  │              │    │                     │  │
                │  │ goal         │    │ observed_reads      │  │
                │  │ scopes       │    │ focus               │  │
                │  │ status       │    │ last_tool/target    │  │
                │  └──────┬───────┘    └──────────┬──────────┘  │
                │         │                       │             │
                │         └───────────┬───────────┘             │
                │                     ▼                         │
                │         ┌─────────────────────┐              │
                │         │ EvaluationEngine    │              │
                │         │                     │              │
                │         │ graph_distance(     │              │
                │         │   declared, peer)   │              │
                │         │ scope_contains(     │              │
                │         │   declared, target) │              │
                │         │ claim_held_by_peer( │              │
                │         │   target)           │              │
                │         └──────────┬──────────┘              │
                │                    ▼                         │
                │         GREEN / YELLOW / RED                 │
                │                                               │
                │  PresenceRegistry (existing)                 │
                │  OccupancyMap    (existing)                 │
                │  claims          (RED-path enforcement)      │
                └───────────────────────────────────────────────┘
```

## Data model

### `Intent`

```rust
pub struct Intent {
    pub id: IntentId,
    pub agent_id: AgentId,
    /// Free-form goal text from the agent. Not parsed.
    pub goal: String,
    /// Symbol-level scopes the agent intends to modify. Paths are
    /// canonicalized through the same pipeline as `ClaimRequest.path`
    /// so a `auth::validate_token` scope collides with a peer's
    /// `auth::validate_token` regardless of which side wrote the
    /// absolute or relative form.
    pub scopes: Vec<String>,
    /// One of: `Investigating`, `Planning`, `Editing`, `Reviewing`,
    /// `Done`. The status does not gate any tool — it's a hint for
    /// other agents reading the activity feed.
    pub status: IntentStatus,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

pub struct IntentId(pub String); // ULID
```

`IntentRegistry` mirrors `PresenceRegistry`: `Arc<Mutex<HashMap<IntentId, Intent>>>`
plus `by_agent: HashMap<AgentId, IntentId>` so each agent has at most
one active intent. Updating an intent replaces the entry; the
`updated_at` advances.

### `Activity`

Per-agent live activity derived from hook events. Lives on the same
registry as `Intent` (single `Arc<Mutex<ActivityTracker>>` keyed by
`AgentId`) so the activity feed and the intent feed share a lock and
a persistence callback.

```rust
pub struct Activity {
    pub agent_id: AgentId,
    /// Tools called in the recent past (capped ring buffer, ~100
    /// entries). Used to derive `observed_reads` and `focus`.
    pub recent_tools: VecDeque<ObservedTool>,
    /// Files the agent has read in this session (deduped, ordered).
    pub observed_reads: Vec<String>,
    /// Symbols most recently touched, computed from the recent tool
    /// stream (`Read auth.rs` → `auth::validate_token` etc.). Pure
    /// derivation; not stored directly.
    pub focus: Vec<String>,
    /// Last tool call and target — drives the activity feed rendering.
    pub last_tool: Option<ObservedTool>,
}

pub struct ObservedTool {
    pub tool: String,           // "Read" | "Grep" | "Bash" | "Edit" | ...
    pub target: Option<String>, // "src/auth.rs" or command line
    pub at: SystemTime,
}
```

### `CoordinationEvaluation`

Returned by the pre-edit hook evaluator.

```rust
pub enum CoordinationLevel {
    Green,
    Yellow { reason: YellowReason, related: Vec<RelatedActivity> },
    Red    { reason: RedReason,    holder: Option<AgentId> },
}

pub enum YellowReason {
    OutsideDeclaredScope { declared: Vec<String>, attempted: String },
    PeerIntentNearby     { distance: u32, peer: AgentId },
    PeerIsReading        { peer: AgentId, file: String },
}

pub enum RedReason {
    ExclusiveClaimHeld { holder: AgentId, lease: ClaimId },
}
```

The level flows back to the agent as a JSON envelope from the pre-hook
endpoint. RED blocks execution; YELLOW is informational and surfaces a
human-readable reason plus the related activity the agent can choose
to consult.

## MCP surface

### `lain_intent`

```json
// Request
{
  "session_token": "...",
  "agent_id": "...",
  "goal": "Add refresh-token validation",
  "scopes": ["auth::validate_token", "token::RefreshToken"],
  "status": "Editing"  // optional, default "Planning"
}

// Response
{
  "intent_id": "I-184",
  "revision": 912,
  "coordination": {
    "level": "yellow",
    "reason": "peer_intent_nearby",
    "distance": 1,
    "related": [{
      "agent_id": "codex-9a",
      "goal": "Change SessionClaims serialization",
      "scopes": ["session::SessionClaims"]
    }]
  }
}
```

Update calls take `intent_id` plus optional `add_scopes`,
`remove_scopes`, `goal`, `status`. The server returns the same shape
so the agent can read the new coordination level without a second
round trip.

### `list_active_intents`

Returns the per-agent activity model:

```json
{
  "intents": [
    {
      "agent_id": "alice",
      "goal": "Add refresh-token validation",
      "status": "Editing",
      "scopes": ["auth::validate_token"],
      "focus": ["auth::validate_token", "token::RefreshToken"],
      "observed_reads": ["src/auth.rs", "src/token.rs"],
      "last_tool": { "tool": "Read", "target": "src/token.rs" }
    }
  ]
}
```

### `who_am_i` and `list_active_agents` — extended

Add `goal`, `status`, `scopes`, `focus`, `observed_reads` to the
existing per-agent payload. The activity feed is the *primary*
cross-agent surface; the existing per-agent identity is the secondary
one.

## Hook protocol

The hook layer is the path through which Lain observes the agent's
work. Each agent kind has its own hook surface — Claude Code reads
JSON from stdin in its `PreToolUse` hook, AGY has a different
contract, etc. — but the wire shape inside the hook is identical:

```json
{
  "session": "claude-7f31",
  "event": "tool_start" | "tool_end" | "session_start" | "session_end"
          | "user_prompt" | "subagent_start" | "subagent_end",
  "tool": "Read" | "Grep" | "Edit" | "Bash" | "Write" | ...,
  "target": "src/auth.rs",
  "at": "2026-09-20T17:42:01.123Z"
}
```

Lain exposes a single hook endpoint: `POST /hook` that accepts the
shape above and routes to the activity tracker. The hook itself is a
thin shell wrapper (`hooks/claude-code/`, `hooks/agy/`, etc.) that
serializes the agent-kind-specific event into the wire shape and
POSTs it.

### Per-agent-kind installations

- **Claude Code** — `claude-code` settings.json registers a
  `PreToolUse` hook that calls the shell wrapper. The wrapper exits
  with the coordination level (`GREEN`/`YELLOW`/`RED`) encoded in the
  exit code or stdout JSON so Claude Code blocks RED.
- **AGY** — register a hook plugin. Same wire shape.
- **Cursor / Codex / Kimi** — equivalent per-agent wiring, each with
  its own `hooks/<kind>/` directory.

`lain setup --agent claude` writes the Claude Code settings.json
fragment and the system-prompt snippet. Other agent kinds ship with
their own `setup` commands.

### Pre-edit evaluation

The `PreToolUse` hook for `Edit` (and `Write`, `MultiEdit`) does the
coordination check:

1. Read the agent's current `Intent` from the registry. If none,
   return `YELLOW { reason: OutsideDeclaredScope }` so the agent is
   nudged toward declaring intent rather than editing blind.
2. Canonicalize the target path; resolve the symbol(s) the edit
   touches via the static graph (this is what `get_blast_radius` and
   `query_graph` already do).
3. Check whether the target (path or symbol) appears in
   `Intent.scopes`. If not, return `YELLOW { reason: OutsideDeclaredScope }`.
4. For each peer intent, compute the graph distance between declared
   scopes and the target. Distance ≤ 2 → `YELLOW { reason: PeerIntentNearby, distance }`. Distance 0 → `RED { reason: ExclusiveClaimHeld }` if the peer holds a claim, else `YELLOW { PeerIntentNearby distance: 0 }`.
5. For each peer activity entry, if the peer is currently reading the
   target file, return `YELLOW { reason: PeerIsReading }`.
6. Otherwise return `GREEN`.

The hook returns synchronously so the agent blocks on RED. The
response includes the `related` array the agent can render.

## System prompt

Installed by `lain setup --agent claude` (and equivalents):

```
You are operating in a Lain-managed workspace. Lain coordinates
across agents via declared intent and automatic observation.

Before a substantial code change, declare your goal and the
scopes you intend to modify via `lain_intent`. Update the intent
when your scope materially changes. Do not report individual
reads or commands — Lain observes those through hooks.
```

Three sentences. Nothing else.

## Persistence

The activity tracker and intent registry persist the same way as
the existing presence/occupancy snapshot: a JSON file under
`XDG_STATE_HOME`, written by an inline persist callback. The new
fields do not change the file format — `intent` and `activity` keys
sit alongside `presence` and `occupancy`. Old state files load with
empty intent/activity maps via `#[serde(default)]`.

## File-level changes

| File | Why |
|---|---|
| `src/server/intent.rs` (new) | `Intent`, `IntentRegistry`, `IntentStatus` |
| `src/server/activity.rs` (new) | `Activity`, `ObservedTool`, ring buffer |
| `src/server/evaluation.rs` (new) | `CoordinationLevel`, `YellowReason`, `RedReason`, `evaluate(intent, target, peers, claims)` |
| `src/server/mcp/intent_tools.rs` (new) | `run_lain_intent`, `run_list_active_intents` |
| `src/server/mcp/handler.rs` | Register `lain_intent` and `list_active_intents` in the dispatcher; extend `who_am_i` / `list_active_agents` payloads with `goal`, `status`, `scopes`, `focus`, `observed_reads` |
| `src/server/ingest/handles/presence.rs` | Add `IntentRegistry` and `ActivityTracker` to `PresenceLayer` |
| `src/server/mcp/handler.rs::handle_request` | Add `POST /hook` route |
| `src/cli/setup.rs` | Install the system-prompt snippet and the Claude Code settings fragment |
| `hooks/claude-code/` (new or extended) | Hook wrapper that POSTs to `/hook` and exits with the coordination level |
| `hooks/agy/` | Same shape, AGY-specific event sourcing |
| `docs/multiplayer.md` | Rewrite for the intent layer (this plan supersedes the "claim arbitration" section) |
| `docs/USER_MANUAL.md` | Document `lain_intent` and the activity feed |
| `docs/hooks.md` (new) | Wire shape, per-agent-kind installation, pre-edit evaluation rules |
| `docs/FOLLOWUPS.md` | Link to this plan and to the regression test that asserts the GREEN/YELLOW/RED levels |

## Out of scope (tracked, not in this plan)

- Server-side enforcement that the agent cannot bypass the pre-hook
  (the hook is a soft gate; agents that don't honor it can still
  edit). The RED path exists because the file-lock primitive is
  authoritative for exclusive claims; for non-claim coordination the
  agent has to honor the hook itself.
- Inference of intent from the user prompt. Observed but not
  authoritative.
- Auto-derivation of `scopes` from observed tool calls (would let an
  agent skip `lain_intent` and still get sensible coordination). The
  system prompt nudges toward declaring; the hook returns YELLOW
  when no intent is declared.

## Tests

### Unit

- `IntentRegistry::upsert_replaces_by_agent` — a second `upsert` for
  the same agent replaces the existing entry, advancing `updated_at`.
- `Activity::record_tool_appends_to_recent` — ring buffer caps at
  100 entries; oldest is dropped.
- `evaluate` table-driven tests over:
  - target inside declared scope, no peers → GREEN
  - target outside declared scope → YELLOW OutsideDeclaredScope
  - target inside declared scope, peer has graph-distance-1 intent
    → YELLOW PeerIntentNearby distance=1
  - target inside declared scope, peer holds exclusive claim on the
    same symbol → RED ExclusiveClaimHeld
  - target inside declared scope, peer is currently reading the file
    → YELLOW PeerIsReading

### Integration

- Two agents register, agent A declares intent on `auth::validate_token`,
  agent B's hook evaluator against an Edit on the same symbol returns
  YELLOW with `related` pointing at A's intent.
- Three-agent scenario: A's intent is on `auth::*`, B's intent is on
  `session::*`, B's pre-edit hook on `auth.rs` returns GREEN (B's
  declared scope is session, but the edit is outside it so we expect
  YELLOW OutsideDeclaredScope). Edge case the table-driven test
  catches.
- The existing `tests/multi_agent_concurrency.rs::two_agents_race_claim_one_wins_one_conflicts`
  test still passes — the intent layer does not change the
  linearizability invariant.

### Regression / chaos

- 1000-iteration N=10 stress: each iteration has 10 agents declare
  intent on overlapping scopes; assert exactly one acquires the
  exclusive claim (RED path), the other nine see YELLOW.
- Kill the holder mid-iteration: the next agent's pre-edit hook
  returns GREEN after expiry, YELLOW before.
- Corrupt the activity file: the evaluator returns YELLOW
  OutsideDeclaredScope (no scope available) rather than GREEN.

### AGY harness

End-to-end run:

1. Start `lain server --transport http --port 9999` for the run's
   workspace.
2. Two AGY agents run with the hook wrapper; each calls `lain_intent`
   to declare their goal.
3. The harness asserts the cross-agent activity feed matches the
   declared intent (`list_active_intents`).
4. Agent A attempts an Edit inside its scope → GREEN; outside →
   YELLOW.
5. The agent's report (`verdict.json`) records the per-iteration
   level outcomes.
6. The harness workdir defect (shell commands running from the wrong
   cwd) is fixed in the same PR; assert
   `git rev-parse --show-toplevel` matches the run's cwd.

## Acceptance criterion

The work is complete when:

1. An agent can declare intent via `lain_intent(goal, scopes, status)`
   and the response carries `intent_id`, `revision`, and a
   `coordination` level with related activity.
2. `list_active_intents` returns the activity model for every
   connected agent.
3. The pre-edit hook returns GREEN / YELLOW / RED based on the
   evaluation rules in `evaluation.rs`, with reasons and related
   activity populated.
4. The system-prompt snippet is installed by `lain setup --agent
   claude` and visible to the agent on session start.
5. The 1000-iteration N=10 stress test never produces two successful
   grants on the same exclusive scope (the linearizability invariant
   from the prior coordination plan).
6. The AGY harness end-to-end run produces a `verdict.json` that
   records per-iteration levels and matches the agent's reported
   activity.
7. Existing `tests/multi_agent_concurrency.rs` still passes — the
   intent layer adds, it does not change the claim primitive.
8. Rust tests, Clippy, formatting, and the dev CI lane pass.

## Order of work

This plan lands across multiple PRs to keep each diff reviewable.

**PR 1 — server-side intent types and tools:**
- `src/server/intent.rs`, `src/server/activity.rs`
- `src/server/mcp/intent_tools.rs` (`run_lain_intent`,
  `run_list_active_intents`)
- Extend `who_am_i` / `list_active_agents` payloads
- Persist the new fields in the snapshot file
- Unit tests for the registry and activity ring buffer

**PR 2 — hook ingestion endpoint:**
- `POST /hook` route in `src/server/mcp/handler.rs`
- `hooks/claude-code/` shell wrapper that POSTs to `/hook`
- Activity tracker records events
- Integration test: a Claude Code hook script running against a
  temp workspace populates the activity feed

**PR 3 — pre-edit evaluation engine:**
- `src/server/evaluation.rs`
- Hook wrapper returns the coordination level synchronously
- Table-driven tests for the GREEN/YELLOW/RED rules
- Two-agent integration test from the plan's test section

**PR 4 — system prompt and setup wiring:**
- `src/cli/setup.rs` writes the system-prompt snippet and the
  Claude Code settings fragment
- Test that `setup --agent claude --print-config` produces the
  expected fragment

**PR 5 — AGY harness end-to-end:**
- `hooks/agy/` wrapper
- Update the AGY harness to use the new hook protocol
- The workdir fix
- `verdict.json` recording per-iteration levels

**PR 6 — docs:**
- `docs/multiplayer.md` rewrite
- `docs/USER_MANUAL.md` update
- `docs/hooks.md` new
- `docs/FOLLOWUPS.md` link

## Open questions deferred

- Does the pre-edit hook need to be authoritative (server-side
  enforcement) or advisory (the agent honors the level)? The
  current design treats it as advisory for YELLOW and authoritative
  for RED (because RED corresponds to an existing claim, which is
  enforced by the file-lock primitive). GREEN is no-op.
- Should the activity feed be visible to a peer without an
  `register_agent` round-trip? The current plan keeps the existing
  registration requirement.
- Should `scopes` accept both paths and symbols, or symbols only?
  The example in the user's message uses symbols (`auth::validate_token`)
  but the existing claim API uses paths. The plan accepts both:
  symbol forms go through the graph; path forms go through
  path-canonicalization. The two are unified at the evaluator level
  (everything is resolved to a graph node before evaluation).