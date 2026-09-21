# Coordination consistency plan

**Date:** 2026-09-20  
**Status:** design, not implemented  
**Scope:** cross-process presence, occupancy, and edit-claim arbitration

## Problem statement

Lain supports more than one MCP server process for a workspace. Each process
keeps a live `PresenceRegistry` and `OccupancyMap`, while a JSON snapshot on
disk carries sessions and claims between processes. An edit claim must not be
granted from an older snapshot when another process already owns the path.

The real-agent run `real-20260920-2` produced the failure this plan addresses:
the holder and contender both received an edit grant for
`lain-contention-canary.txt`. The run recorded two `ClaimGranted` events and no
`ConflictDetected` event. The repository's own two-process race test passes
against the installed binary, so the failure is intermittent rather than a
simple statement that stdio cannot share state.

The current design intentionally proceeds without a state lock after a timeout
or filesystem error. That choice keeps an advisory tool call from hanging, but
it also permits two stale read-modify-write cycles to grant the same edit.
That is acceptable for a read listing; it is not acceptable for `claim_files`.

## Current code path

### State-file location

`src/config/mod.rs::state_path_for_workspace` derives a per-workspace JSON path
from the canonical workspace path. The filename contains a sanitized stem and
the first eight hexadecimal characters of a BLAKE3 digest, which prevents two
same-named workspaces from sharing a snapshot accidentally.

`XDG_STATE_HOME` controls the parent directory. A test or harness can therefore
give several processes the same isolated state root without touching the
developer's normal state.

### Server startup

`src/server/ingest/constructors.rs` builds the server, then calls:

1. `LainServer::load_state()` to hydrate presence and occupancy.
2. `LainServer::install_persist_callback()` to attach callbacks to both maps.

The callbacks call `save_presence_pair` after mutations. The save path writes
an atomic replacement of the JSON snapshot.

### Mutation path

`src/server/mcp/presence_tools.rs::run_claim_files` calls
`LainServer::with_shared_presence` before `run_claim_files_inner`.

`with_shared_presence` currently does this:

1. Derive the state-file path.
2. Call `state_lock::acquire`.
3. Call `refresh_shared_presence`.
4. Run the mutation.
5. Drop the lock.

`run_claim_files_inner` authenticates the session, builds `ClaimRequest` values,
calls `OccupancyMap::claim_with_session`, emits `ClaimGranted` events, and lets
the occupancy persist callback save the resulting state.

### Refresh optimization

`src/server/ingest/handles/presence.rs::refresh_shared_presence` compares the
state file's modification time with `presence_state_seen`. If the timestamp has
not changed, it skips parsing. Otherwise it calls `load_state`, which replaces
the in-memory maps with the snapshot rather than merging stale entries into the
live maps.

That optimization is valid only when the process owns the state lock during a
mutation. Without the lock, the timestamp check and the subsequent write can
still race with another process.

### Lock implementation

`src/server/state_lock.rs` creates an `O_EXCL` sentinel beside the JSON file.
The default values currently come from `PresenceConfig::default()`:

- acquire timeout: 2,000 ms;
- retry interval: 20 ms;
- stale-lock takeover: 10 seconds.

`StateLock` records whether acquisition succeeded, but callers do not inspect
that flag. On timeout, an unavailable state directory, or another lock error,
`acquire` returns an unlocked guard and the caller continues.

There is also a configuration defect: `state_lock::acquire` creates a fresh
default `PresenceConfig` instead of receiving the already loaded server
configuration. Values in `.lain/tuning.toml` therefore do not affect these
three timings.

## Target behavior

The system must preserve these invariants:

1. Two edit claims for the same canonical path cannot both return `granted`.
2. A mutating operation either commits its state update while holding the lock
   or returns an error; it must not claim success after an unlocked write.
3. A lock failure must not delete or overwrite another process's state.
4. Read-only calls may use a best-effort refresh and may report a documented
   stale result.
5. A released claim becomes visible to another process on its next mutation.
6. Session expiry and unregister cleanup release the session's occupancy and
   filesystem leases.
7. If a caller sends `ttl_seconds`, Lain must enforce it or reject the request;
   silently dropping the field is not allowed.

The word “advisory” describes the fact that Lain cannot prevent an agent from
editing a file after a claim is refused. It must not describe the arbitration
result itself. Arbitration must be consistent.

## Proposed design

### 1. Make lock failure visible

Change the lock API so callers can distinguish an acquired lock from an
unavailable lock. The exact Rust type can vary, but it must carry a reason and
the ownership state. Do not represent timeout as a normal unlocked
`StateLock` for mutation paths.

Suggested shape:

```rust
pub enum StateLockResult {
    Acquired(StateLock),
    Unavailable { reason: String },
}
```

Keep stale-lock takeover. A stale sentinel is not a failure when the takeover
successfully creates a new sentinel.

### 2. Separate read and mutation policies

Use two wrappers or an explicit policy argument:

- `with_shared_presence_read`: refresh when possible; continue with a stale
  read if the state file is unavailable.
- `with_shared_presence_mutation`: require the lock, reload under the lock,
  run the mutation, and persist before releasing it.

At minimum, the following calls need the mutation policy:

- `register_agent`;
- `heartbeat`;
- `claim_files`;
- `release_files`;
- unregister and session-removal cleanup.

`list_active_agents`, `list_occupancy`, `who_am_i`, and similar listings can
retain best-effort reads, provided their documentation says that a result can
lag a peer process.

### 3. Return a stable coordination error

When a required lock cannot be acquired, return a machine-readable error such
as:

```json
{
  "error": {
    "code": "coordination_unavailable",
    "message": "Lain could not lock the shared presence state; retry the operation."
  }
}
```

The MCP transport should preserve the error code. The message can include the
state path and lock reason in logs, but it should not expose session tokens.

### 4. Pass real tuning into the lock layer

The server already loads tuning during construction. Store or pass the
`PresenceConfig` values needed by `state_lock::acquire`; do not call
`PresenceConfig::default()` inside the lock module.

Add tests that set a short timeout and retry interval, then assert the observed
lock behavior matches those values. This guards against a configuration field
that exists in the schema but has no runtime effect.

### 5. Make persistence part of mutation success

The current persist callback logs save failures and lets the mutation return a
success response. For a claim or release, that can tell the caller it owns a
path even though another process will never observe the update.

Choose one of these designs and document it:

- return persistence errors from mutation tools; or
- make the mutation callback return `Result`, and only emit success events after
  the atomic save completes.

The second option gives the cleanest event contract: `ClaimGranted` means the
state update reached disk while the lock was held.

### 6. Implement claim TTL instead of discarding it

`ClaimFilesEntry` currently deserializes `path`, `symbols`, `intent`, and
`plan_revision`, while `run_claim_files_inner` constructs every request with
`ttl_seconds: None`. The harness sends `ttl_seconds: 120`, but serde ignores
that unknown field.

Add an optional `ttl_seconds` field, validate its bounds, and pass it to
`ClaimRequest`. If the API does not want per-request TTL yet, reject the field
with a clear schema error and update the harness. Do not silently accept and
discard it.

## Test plan

### Unit tests

Add coverage for:

- acquired lock release on drop;
- timeout returns `coordination_unavailable` for mutation calls;
- stale sentinel takeover;
- unavailable state directory does not grant a mutation;
- configured timeout and retry values are used;
- persistence failure does not emit a success event;
- TTL validation, expiry, and serialization.

### Two-server in-process test

Keep `tests/shared_presence.rs` and extend it to cover the error path. It
already creates two independent `LainServer` values over one workspace and
checks claim, listing, and release visibility.

### Real child-process test

Extend `tests/multi_agent_concurrency.rs` or add a focused companion test:

1. Create a temporary Git workspace and a temporary `XDG_STATE_HOME`.
2. Spawn two real `lain mcp --workspace ...` children.
3. Register both agents.
4. Synchronize their claim requests with a barrier.
5. Assert one edit grant and one conflict.
6. Hold the winner's claim while the loser retries; every retry must either
   report the conflict or return `coordination_unavailable`, never a grant.
7. Release from the winner and assert the loser can claim afterward.
8. Repeat the sequence at least 20 times.

The harness should use the same isolated state root for all clients. It should
not depend on the developer's global `~/.local/lain/state` directory.

### Real-agent harness

Retain the shared HTTP run because it gives one observable server and already
produced a conflict in `real-shared-20260920-2`. Before treating it as a full
pass, fix the AGY launcher so shell commands execute inside the disposable
checkout; assert the agent's reported `git rev-parse --show-toplevel` matches
the run metadata. Then run both implementation agents, contention agents, and
the independent verification step, producing a complete `verdict.json`.

The original stdio run remains valuable evidence. It exercises the path that
failed in practice; the new fail-closed behavior should turn that failure into
an explicit coordination error rather than a double grant.

## Documentation changes

Update these files after the code and tests land:

- `docs/multiplayer.md`: distinguish consistent claim arbitration from
  advisory filesystem enforcement; document `coordination_unavailable` and the
  read-versus-mutation behavior.
- `docs/USER_MANUAL.md`: document lock tuning as active configuration, not just
  accepted syntax; document claim TTL behavior.
- `docs/FOLLOWUPS.md`: track the defect with links to the regression test and
  the real-agent evidence run.
- Harness README and prompts: state that HTTP is recommended for a shared
  multi-agent run, while stdio remains supported through the shared state file.

## Rollout sequence

1. Land the lock-result and mutation-policy changes.
2. Land configuration plumbing and error-contract tests.
3. Land TTL support or remove the field from all callers.
4. Run the focused Rust suites and repeated child-process test.
5. Run the real-agent harness with isolated state and corrected AGY working
   directory.
6. Run the normal dev CI lane.
7. Only then consider including the changes in a release branch.

No version bump belongs in the implementation work; release metadata follows
the repository's release policy.

## Non-goals

This plan does not turn Lain claims into operating-system file locks. An agent
can still ignore a refused claim and edit the file. It also does not replace
the shared HTTP server with a new database or daemon. The first fix should make
the existing state-file design honest and safe for mutation calls.

## Acceptance criteria

The work is complete when all of the following hold:

- no repeated two-process test grants two competing edit claims;
- lock timeout and state-write failures produce explicit mutation errors;
- configured lock timings affect runtime behavior;
- `ttl_seconds` is enforced or rejected;
- release and expiry remain visible across processes;
- the harness runs both agents in their disposable repositories;
- the evidence run writes a passing `verdict.json`;
- Rust tests, Clippy, formatting, and the dev CI lane pass.
