> **Note:** This document supersedes `docs/superpowers/plans/2026-08-14-lain-consolidation.md` for the multiplayer layer. The 2026-08-14 plan deleted `hook.rs`, `projects.rs`, and the owner/sidecar/lock machinery as "multi-user coordination"; the multiplayer work (PRs 8–11) rebuilt those surfaces with single-user + multiple-agents semantics.

# Multiplayer Awareness

> lain 0.5+ ships an always-on multiplayer layer so multiple agents (Claude,
> Kimi, Cursor, OpenCode, humans) can edit the same workspace without
> stomping on each other. This page is the operator + agent quickstart.

## Operator quickstart

Multiplayer is always on. Start `lain server` like you already do — the
extra surface is 8 MCP tools and an SSE feed that the existing Command
Center picks up automatically:

```bash
# Start lain with the multiplayer features enabled (always-on in v0.5+)
lain server --config ./repos.yaml --transport http --port 9999
```

What's new on the wire:

| Surface | Purpose |
|---|---|
| 8 new MCP tools | `register_agent`, `heartbeat`, `list_active_agents`, `who_am_i`, `claim_files`, `release_files`, `list_occupancy`, `my_claims` |
| `GET /events` | Server-Sent Events stream. Fires whenever an agent joins, claims, releases, or a conflict is detected. The Command Center subscribes here for its live panels. |
| Command Center panels | `GET /` now shows the **Agents online** and **Rooms** (file occupancy) panels, updated live. |
| Existing tools | `query_graph`, `get_repo_info`, `explain_symbol`, and `get_cross_repo_blast_radius` now include attribution hints (active editors) and surfaces occupancy where it's relevant. |

No new flags. No new dependencies. If your existing `repos.yaml` works,
multiplayer works.

## Agent quickstart

Every agent that wants to participate goes through this 4-call dance:

```js
const { agent_id, session_token, expires_at_unix } = await mcp.call("register_agent", {name: "claude", kind: "claude-code", pid: process.pid});
// Heartbeat every 30s while the agent is alive.
setInterval(() => mcp.call("heartbeat", {agent_id, session_token}), 30_000);

// Before editing auth.rs:42-78, claim it.
const { granted, conflicts } = await mcp.call("claim_files", {agent_id, session_token, files: [{path: "auth.rs", symbols: ["login"], intent: "edit"}]});
if (conflicts.length > 0) {
  // Coordinate or pick different scope.
  for (const c of conflicts) console.warn(`Agent ${c.name} is working on the same scope`);
}

// After editing:
await mcp.call("release_files", {agent_id, session_token, files: [{path: "auth.rs"}]});
```

The four calls in plain English:

1. **`register_agent`** — once at startup. lain assigns an
   `agent_id` (UUID) and a `session_token` (opaque hex string). The
   token is the agent's bearer credential for every subsequent call;
   keep it secret.
2. **`heartbeat`** — every 30s. lain expires sessions 60s after the
   last heartbeat, so a 30s cadence means a missed heartbeat still
   buys you one retry window before eviction.
3. **`claim_files`** — before editing. Pass an array of
   `{path, symbols?, intent?}` entries. `intent` is `edit` or `read`;
   `symbols` is an optional list of symbol names to scope the claim
   finer than the file. lain returns `{granted, conflicts}` — if
   `conflicts` is non-empty, another agent holds a competing claim.
4. **`release_files`** — after editing. Frees the claim so the next
   agent can pick it up.

You can also poll without claiming — `list_occupancy` (optionally
scoped to a path) reports who currently holds claims on a file, and
`list_active_agents` enumerates every connected session.

### Listening for events (push)

Connect to the SSE stream and lain will keep you in the loop — no
polling required:

```bash
curl -N http://localhost:9999/events
# event: ready
# data: {}
#
# event: agent_joined
# data: {"agent_id":"5f74f...", "name":"claude", "kind":"claude-code"}
#
# event: claim_granted
# data: {"agent_id":"5f74f...", "path":"auth.rs"}
```

Event types: `ready` (initial sync), `agent_joined`, `agent_left`,
`claim_granted`, `claim_released`, `conflict_detected`. The Command
Center consumes this stream verbatim.

## How attribution works (defensive layer)

If an agent forgets to call `claim_files`, lain still knows:

1. **Inotify** watches the workspace. Any file change is detected.
2. **`/proc/<pid>/fd`** looks for the writer's PID. If a registered agent has that PID, lain auto-claims on their behalf.
3. **Single-agent fallback**: if only one agent is connected, edits are attributed to them.
4. **Audit log**: unattributed edits are logged to stderr so the operator can investigate.

## Stability & persistence

Symbol IDs include a BLAKE3 content hash. Claims carry an optional `ttl_seconds` that bounds them even with active heartbeats. State (`PresenceRegistry` + `OccupancyMap`) is persisted to `~/.local/lain/state/<config-stem>-<hash>.json` (hash of the absolute config path, so two configs that share a filename stem don't collide) on every mutation and restored on `lain server` startup, so claims survive restarts.

For attribution portability, lain selects the backend at startup:

| OS | Backend | Notes |
|---|---|---|
| Linux | `ProcFsBackend` | Walks `/proc/<pid>/fd` for the writer's pid. |
| macOS | `LsofBackend` | Shells out to `lsof -F p`. Falls back to `NoopBackend` if `lsof` is missing. |
| Windows | `NoopBackend` | Always returns `None`. lain falls back to git polling + single-agent heuristic. |

Disable process attribution entirely with `--no-process-attribution`:

```bash
lain server --config ./repos.yaml --transport http --port 9999 --no-process-attribution
```

## Subagents

When an agent spawns subagents (e.g., Claude Code's Task tool), each
subagent should appear in `lain` as a distinct session whose
`parent_session_id` points to the spawning session. To wire this:

1. The parent registers first (no `parent_session_id`); capture its `agent_id`.
2. Spawn the subagent with the environment variable `LAIN_PARENT_AGENT_ID=<parent.agent_id>` set.
3. The subagent's `PreToolUse` hook passes that through `lain hooks claim --parent-session-id "$LAIN_PARENT_AGENT_ID"`.

The subagent can introspect its parent via `who_am_i` (which now
includes `parent_session_id`). The parent can enumerate its subagents
via `list_subagents` (passing its own session token).

## Commit-time overlap detection

The MCP tool `detect_overlap` finds symbol-level conflicts between two git refs in the active workspace:

```bash
lain detect_overlap --base HEAD~1 --head HEAD --workspace backend
```

Returns:

```json
{
  "base": "abc123",
  "head": "def456",
  "files": [
    {
      "path": "src/auth.rs",
      "symbols_base": ["login", "validate"],
      "symbols_head": ["login", "logout"],
      "overlap": ["login"],
      "severity": "medium"
    }
  ],
  "total_overlaps": 1
}
```

Pair with `hooks/claude-code/pre-commit.sh` (configured as Claude Code's `PreToolUse` on `Bash` for `git commit`) to refuse commits that would conflict with the previous ref. Catches the real damage — merge conflicts — at the right moment.

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
