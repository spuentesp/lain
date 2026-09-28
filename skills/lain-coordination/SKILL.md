---
name: lain-coordination
description: Use when working alongside other agents with Lain — "claim these files", "who else is here", "will we collide", "leave a note for the next agent", "what happened while I was away". Covers the session, social, and notes packages.
---

# Lain coordination — work beside other agents

Multiplayer plumbing is hidden from the default surface because
**hooks usually drive it** — `hooks/<agent>/` scripts call
`claim_files` / `heartbeat` / `release_files` directly. Tools are
callable by name even when hidden, but `load_package` makes them
appear in `tools/list`; many MCP clients only permit tools their
cached list contains. Load these packages only when *you* are
managing the coordination yourself.

## Load them

`load_package("session")` for claiming, `load_package("social")`
for the roster, `load_package("notes")` for handoffs — or set
`LAIN_TOOL_PROFILE=session,social,notes` at config time for a
coordinator agent. Reconnect the client if the tools do not appear.

## Claim discipline (session)

1. `register_agent` once (name yourself meaningfully).
2. Before editing: `detect_overlap` on the files/symbols you will
   touch — if a peer holds them, coordinate first.
3. `claim_files` with an intent and a TTL you can honour; refresh
   with `heartbeat`; `release_files` the moment you are done.
   `my_claims` / `list_occupancy` / `get_world_state` answer
   "who holds what" at any moment.

## Awareness (social)

`list_active_agents` (who is here), `who_am_i` (what the server
thinks you are), `list_subagents`, `get_audit_log` (recent
coordination events in this workspace), `unregister_agent` on exit.

## Handoffs (notes)

`leave_handoff_note` when your work continues elsewhere — say what
you changed and what you did NOT do. `get_pending_handoffs` when you
start. `lain_intent` / `list_active_intents` for the plan board;
`add_annotation` / `list_annotations` / `resolve_annotation` for
notes pinned to code.

Example payloads (copy verbatim, fill in values):
```
add_annotation  →  target={"kind":"symbol","symbol":"MyFn","repo_id":"..."},
                   kind="note", body="..."
leave_handoff_note  →  body="[scope:auth] did X; Y still open", scope="auth"
```

## Rules of thumb

- Claims are advisory coordination, not locks — still re-check
  before you write.
- Keep TTLs short and heartbeats honest; an expired claim drops
  (by design) and a stale one is worse than none.
- Never claim files "just in case" — claim when you are about to
  edit.
