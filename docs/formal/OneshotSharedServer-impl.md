# B1: Shared `lain mcp` per workspace — implementation design

Companion to `OneshotSharedServer.tla` (the formal spec).

## Goal

`lain oneshot` currently spawns a fresh `lain mcp` per call, paying
~5 min of cold reindex for a 41k-LOC repo on every invocation.
After this change, an `oneshot` call consults a per-workspace
socket first; if a server is alive it connects (cheap), and only
spawns a fresh one when the previous one is dead.

## Topology

```
  ~/.local/lain/run/<workspace-hash>.sock     (per-workspace Unix socket)

  oneshot(c1) ──┐
                 ├─► connect ──► lain mcp (running) ──► graph.bin
  oneshot(c2) ──┘     │
                      └─ (if connect fails: spawn lain mcp --socket <path>)
```

The socket file path is derived from the workspace's absolute path:

```
$LAIN_RUN_DIR / lain-mcp-<blake3(workspace).hex[..16]>.sock
```

`$LAIN_RUN_DIR` is `$XDG_RUNTIME_DIR/lain` (Linux) or
`$HOME/.local/lain/run` (fallback), matching the existing
`config::run_dir()` helper.

## CLI changes

`lain mcp` gains a `--socket <path>` flag. When set:

1. The server binds the socket *in addition to* stdio (stdio
   stays for `setup` and the CI case where oneshot spawns).
2. The server's first action is to write the PID to a
   `<socket>.pid` sidecar file. oneshot reads it to confirm the
   process is alive (`kill -0`).

`oneshot` first reads `<socket>.pid`:

| pid file | connect | spawn |
|---|---|---|
| absent | n/a | spawn `lain mcp --socket <path>` |
| alive (kill -0 OK) | succeed | fall back to spawn if connect fails |
| dead (kill -0 fails) | n/a | spawn |

In the spawn case, oneshot polls the socket file with
`connect_timeout=2s, attempts=30` (~60s ceiling) until the
server has bound.

## State changes (mirror the TLA+ spec)

- `socket_path: PathBuf` — the per-workspace path; the run dir +
  a hash of the canonicalized workspace path.
- `pid_file: PathBuf` — `<socket>.pid`; written on bind, removed
  on shutdown.
- The MCP handler already manages per-connection state; the
  socket adds N new connections to the same handler.

## Concurrency invariants (cross-reference TLA+)

| Spec invariant | Rust enforcement |
|---|---|
| S1: at most one server process | `bind()` returns `EADDRINUSE` → exit 1; we don't retry. The pid file is the second line of defense. |
| S2: client bound implies server alive | The server's accept loop is single-threaded; if the server dies, accept returns 0, and oneshot's connect fails fast. |
| S3/S4: serving set consistent | Each accept is a separate handler invocation; no shared state across connections means the spec's `serving` set is per-connection, not cross-connection. |

## Implementation order (commit-by-commit)

1. `feat(oneshot): add config::oneshot_socket_path(workspace)` and
   its unit test.
2. `feat(server): lain mcp --socket <path>`; the stdio path is
   unchanged. The `--socket` flag binds a tokio UnixListener
   in parallel and forwards frames to a second `mcp-server`
   runtime sharing the same executor / federation / workspaces
   / snapshots / state — this is the part that needs a small
   refactor to factor the handler-construction out of
   `run_stdio` so both transports can use it.
3. `feat(oneshot): connect to existing server first, spawn on
   connect failure`. This is the actual B1 fix.
4. `feat(oneshot): graceful shutdown when last client leaves`.
   Optional: TTL-based, or operator-driven (`--oneshot-ttl=0`).

## Out of scope (defer)

- Concurrent write safety across the two transports (stdio +
  socket): both share the same handler so there is no extra
  concurrency beyond what the SDK already provides per
  connection. The TLA+ spec's `serving` set is the per-server
  state, not the cross-server state, and Rust's borrow
  checker prevents the obvious races.
- Federation mode (`--workspace PATH` repeated N times).
  One-shot is single-repo by design; the federation server
  already has its own `run_dir` socket for hot-reload.
- Cross-host transport. Per-workspace sockets are local only;
  remote use was never a goal.
