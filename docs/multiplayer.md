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
