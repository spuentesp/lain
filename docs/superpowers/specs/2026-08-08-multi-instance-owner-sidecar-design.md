> **Status:** Superseded by `docs/superpowers/specs/2026-08-14-lain-consolidation-design.md`.

# Per-Workspace Multi-Instance (Owner + Sidecar)

**Date:** 2026-08-08
**Status:** Design approved for implementation

## Goal

Let multiple `lain` processes coexist on the same workspace: one **owner** that owns the graph, watcher, and write paths, and any number of **sidecars** that read the graph, subscribe to volatile overlay updates, and answer MCP tools without acquiring the writer lock. Today's single-instance lock is the wrong shape: agents like `agy` that spawn a stdio MCP child for the same workspace the HTTP singleton is already running sit at `initializing` forever because the stdio child bails on `<workspace>/.lain/server.lock`.

## Current failure (the one you saw)

`/home/sebastian/orca/workspaces/lain/langostino/src/main.rs:186-198` opens `<workspace>/.lain/server.lock` and bails on collision. The lock is per-workspace, written as `<pid>:<port>`, and the only check is `kill -0`. That is wrong on three axes:

1. PID reuse is real; a crashed owner's PID can be assigned to a new unrelated process.
2. The lock body is text, not an OS-level guard, so two honest processes can race.
3. The lock guards the *whole process* rather than distinguishing the writer-side (`build_core_memory`, watcher, overlay insert) from the read-side (graph queries, overlay read).

The result is that a process running an `agy`-style MCP child for the same workspace the live HTTP singleton is already serving refuses to start, and `agy` sits at `initializing` until its MCP timeout fires.

## Design

### Mode selection

Add `--mode owner|sidecar` to the existing CLI. Default is `owner`, so today's behavior is unchanged. The new `sidecar` mode:

1. Skips workspace lock acquisition (acquires a shared `flock` only to verify the owner is alive, then drops it).
2. Skips `build_core_memory` and `sync_volatile_overlay` (the writer paths).
3. Skips the `FileWatcher` spawn.
4. Opens the graph in read-only mode.
5. Connects to the running HTTP singleton on `LAIN_PORT` for the volatile overlay and for any tool the sidecar cannot satisfy from its own read-only graph cache.
6. Exposes the same MCP `tools/list` and `tools/call` surface as the owner.
7. Subscribes to overlay updates via the new server-stream method.

### Lock

Replace `<workspace>/.lain/server.lock` (text file with `pid:port`) with `<workspace>/.lain/server.lock` (text file still readable for debugging, but the authoritative guard is an `flock(F_SETLK)` on the file). The owner holds an exclusive lock for its lifetime; a sidecar briefly takes a shared lock to verify the owner is alive, then drops it. The old `kill -0` text-only check is removed. The text file is preserved (empty is fine) so the file path is unchanged for any consumer that just wants to know whether the workspace is in use.

### Read-only graph

`GraphDatabase::open_read_only(memory_path)` opens the existing `.lain/graph.bin` as immutable. Every insert/update path is gated on `mode == owner`; the read paths stay unchanged. A sidecar process can open the same graph and query it without affecting the owner. There is no second on-disk format.

### Sidecar command shape

When the user runs `lain agents install --scope user <agent>`, the install loop writes `url: http://localhost:9999/mcp` for agents that support it (Cursor, Kimi, Cline, Continue, OMP, Codex, Antigravity). For agents that do not support `mcpServers.<name>.url`, the install loop writes `command: lain` with `args: ["--mode", "sidecar", ...]`, the same workspace and model flags as the owner, and an env `LAIN_PORT` matching the owner's. Either way, only one `lain` process per workspace actually writes to `.lain/graph.bin`; the rest are clients.

### Overlay updates

The owner adds a new MCP method `overlay/subscribe` that returns a stream of `overlay_diff` events (added/removed/updated). The stream is a JSON-RPC server-stream over the existing HTTP transport. Sidecars open the stream on startup, keep an in-process overlay cache, and merge diffs as they arrive. On disconnect, sidecars reconnect with the last applied revision; brief gaps are tolerated. A polling fallback is supported for clients that cannot keep a long-lived stream open (single-snapshot `overlay/get_snapshot`).

### Watcher

The owner remains the only process that runs the inotify-based file watcher. Sidecars do not register their own inotify file descriptors for the workspace. The watcher is the natural singleton per workspace and the dynamic directory-registration logic is not safe to share across processes.

## Components

- **`LainMode` enum** at `src/main.rs`, parsed from `--mode owner|sidecar`. Default `owner`.
- **`WorkspaceLock` newtype** at `src/lock.rs`, wrapping a `File` with `flock` helpers `acquire_exclusive()` and `acquire_shared()`. Falls back to a `kill -0` advisory message in the shared case for the brief window the sidecar probes the owner.
- **`GraphDatabase::open_read_only`** at `src/graph.rs`, opening the existing `.lain/graph.bin` as immutable. All insert/update paths in `src/server/ingestion.rs`, `src/server/jobs.rs`, and the sensors in `src/sensors/` are gated on `mode == owner`.
- **`Sidecar` runtime** at `src/sidecar.rs`, owning the read-only `GraphDatabase`, the overlay subscription client, and the MCP server. Implements the same `tools/list` and `tools/call` surface as the owner but only against the read-only cache plus the live HTTP singleton.
- **`overlay/subscribe` MCP method** at `src/mcp/handler.rs`, returning a stream of `overlay_diff` events. A polling fallback `overlay/get_snapshot` is also exposed.
- **Install loop changes** at `agents/manifest.toml` and the seven adapter files in `src/cmds/agents/adapters/`, with a `format = "http"` branch and a `format = "sidecar"` branch per adapter.

## Data flow

Owner:
```
CLI argv → LainMode::owner → WorkspaceLock::acquire_exclusive() → LainServer::new
   └ build_core_memory()
   └ sync_volatile_overlay()
   └ FileWatcher::start
   └ background_sync loop
   └ mcp::LainMcpServer::start_http
        └ tools/list, tools/call
        └ overlay/subscribe returns SSE stream from a broadcast channel
```

Sidecar:
```
CLI argv → LainMode::sidecar → WorkspaceLock::acquire_shared() → verify owner is alive → drop
   └ GraphDatabase::open_read_only(.lain/graph.bin)
   └ mcp::LainMcpServer::start_http (read-only)
        └ tools/list, tools/call served from read-only graph + owner via HTTP
   └ SidecarOverlay::subscribe to http://localhost:9999/mcp/overlay/subscribe
        └ on every owner overlay insert → diff applied to local cache
```

## Error handling

- Sidecar cannot acquire a shared `flock`: a real OS-level conflict (not a PID race). Surface as a clear error: another Lain process is starting; retry.
- Sidecar `kill -0` on the owner PID returns non-zero: the owner has crashed. Fall back to polling the on-disk graph without an overlay stream; mark the workspace as `no owner, sidecar is degraded`. Do not auto-promote the sidecar to owner.
- Owner exits while sidecar is connected: sidecar observes the SSE stream close, retries with exponential backoff, and falls back to `overlay/get_snapshot` on a 30-second interval. Sidecar remains a read-only client.
- Owner and sidecar share the same `.lain/server.lock` file. Crashed owner leaves a stale lock; a new owner does `flock(F_SETLK, wait=0)` to discover if the lock is still held; if not, it acquires exclusive. Sidecars do the same with shared.

## Testing

### Automated

- `src/lock.rs` unit tests for `acquire_exclusive`, `acquire_shared`, and stale-lock recovery.
- `src/graph.rs` tests for `open_read_only` and write-rejection.
- `src/sidecar.rs` tests for the read-only path and the overlay subscription client.
- `tests/dual_instance.rs` integration test that:
  1. Spawns one owner and one sidecar on the same workspace.
  2. Asserts both report `Operational`.
  3. Runs a `query_graph` on each and asserts the same answer.
  4. Triggers an overlay insert on the owner; asserts the sidecar receives the diff within 1 second.
  5. Spawns a second `--mode owner` on the same workspace and asserts it fails (the `flock` blocks it).

### Live verification

After all six phases:

1. `cargo test --all-targets` is green.
2. `cargo build --release` succeeds.
3. The live HTTP singleton is up on port 9999.
4. `lain agents install --scope user --all` writes the new HTTP URL form for every agent that supports it, and the sidecar stdio form for the rest.
5. `lain agents verify --all --json` reports `Operational` for every installed agent.
6. Two `agy` (or any two agents) are launched against the same workspace; both load their MCP tools and call `get_health` successfully; the singleton log shows no new `failed to watch workspace` or `channel disconnected` lines.

## Acceptance criteria

- `<workspace>/.lain/server.lock` is a real `flock` guard; the old `kill -0` text check is gone.
- `--mode owner` keeps today's behavior; no test that passes today breaks.
- `--mode sidecar` skips the workspace write paths; opens the graph read-only; starts the HTTP transport; subscribes to the owner's overlay stream.
- Two `lain --mode owner` on the same workspace: the second fails (the `flock` blocks it).
- One `lain --mode owner` plus N `lain --mode sidecar` on the same workspace: all `Operational`; sidecars see overlay updates within 1 second.
- `lain agents install --scope user` writes `url: http://localhost:9999/mcp` for agents that support it, and `command: lain --mode sidecar` for the rest.
- Existing tests in `cargo test --all-targets` remain green; new tests for `WorkspaceLock`, `open_read_only`, and `dual_instance` pass.

## Unchanged behavior

- The HTTP singleton on `LAIN_PORT` remains the default transport.
- The `.lain/graph.bin` format does not change.
- The watcher is owned by a single process per workspace; the owner is the only one that registers inotify file descriptors.
- The `tools/list` and `tools/call` schema for the existing 41 tools does not change.
- The per-agent install loop's `--scope user|project|workspace` semantics do not change.

## Out of scope

- Read-write sidecar mode (a process that holds the writer lock while the owner is suspended). The design is the inverse of the current model and is not required to unblock agents.
- Distributed overlay coherence across multiple owners. The owner is the only writer per workspace; the design is deliberately single-writer.
- Replacing the singleton with a separate coordinator process. The current singleton is the right shape for a single-workspace multi-instance design.
