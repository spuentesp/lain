# Watcher Exclusions and Permission-Tolerant Watching

**Date:** 2026-08-07
**Status:** Design approved for implementation

## Goal

Keep Lain's live file watcher running when a workspace contains unreadable or container-owned directories, while honoring the repository's `.gitignore` rules. A single `EACCES` directory must not disable overlay updates for the rest of the workspace.

## Current failure

`src/watcher.rs` registers the whole workspace once with `RecursiveMode::Recursive`. `notify` performs an initial recursive walk during that call. If it cannot enter a directory such as `infra/neo4j/import`, `watch()` returns `Permission denied`. The watcher thread returns immediately, drops the channel sender, and the async event processor exits on `TryRecvError::Disconnected`.

## Design

### Directory discovery

Add a watcher setup helper that walks the workspace using the existing `ignore` crate with Git ignore support enabled. It will discover readable, non-ignored directories and exclude hidden directories consistently with the current file filter.

Directory discovery errors are handled per entry. An `EACCES` error, or another directory-read failure, is logged with the affected path and skipped. Discovery continues for sibling directories.

### Watch registration

Register each discovered directory with `notify` using `RecursiveMode::NonRecursive`. This avoids a single recursive initial walk crossing an unreadable directory.

A failed registration is logged and skipped; it never returns from the watcher thread. The watcher thread remains alive as long as the process is alive, keeping the event sender connected even when individual directories cannot be watched.

When a create event identifies a new readable, non-ignored directory, the event callback sends a watch request to the watcher thread. The watcher thread performs the registration, avoiding mutation of the `Watcher` from inside the callback. This preserves coverage for newly created source directories.

### Event filtering

File events continue to use the existing source-extension and hidden-path filters. Git-ignore filtering is applied before forwarding a file event to the async processor, so ignored source files do not update the overlay.

Notify callback errors are logged with their details rather than silently discarded. They do not disconnect the processing channel.

### Unchanged behavior

The following remain unchanged:

- 100 ms debounce interval;
- batch size of 20 paths;
- LSP symbol extraction;
- volatile overlay updates;
- existing MCP server and project-registry behavior;
- explicit workspace selection by `lain-server-manager.sh`.

No file permissions will be changed, and no new `.lainignore` syntax will be introduced.

## Testing strategy

### Automated tests

Add focused watcher tests for:

1. Git-ignored directories are absent from the discovered watch set.
2. An unreadable directory is skipped while readable sibling directories remain in the set.
3. A readable source file remains eligible for processing after an unreadable directory is encountered.
4. A newly created readable directory can be registered for watching.

Permission tests will be Unix-gated where necessary and will restore permissions before temporary-directory cleanup.

Run the watcher-specific tests followed by the complete Rust test suite.

### Live multi-client validation

Use one Lain HTTP server and two MCP clients sharing it:

1. Build the changed binary.
2. Restart the project server on port 9999 with the changed binary.
3. Confirm startup logs show the Neo4j import directory being skipped rather than terminating watcher startup.
4. Connect a Kimi Code window and a Claude Code window through their existing MCP proxy configuration.
5. From both clients, confirm health/query access to `monitor_dm_system`.
6. Modify a watched source file outside ignored directories.
7. Confirm the watcher remains alive and the change is visible through the overlay/query path.
8. Confirm the inaccessible Neo4j directory remains untouched and does not produce a channel-disconnected shutdown.

The test will use one server because the manager intentionally enforces a singleton HTTP server on port 9999.

## Acceptance criteria

- Lain starts successfully against `monitor_dm_system` with `infra/neo4j/import` mode `0700`, owned by UID 7474.
- Startup logs contain a per-directory skip warning, not `failed to watch workspace` followed by `channel disconnected`.
- A source edit in a readable, non-ignored directory is observed by the live watcher.
- Kimi Code and Claude Code can use the same running server concurrently.
- Focused tests and the full Rust test suite pass.
- No unrelated files or permissions are changed.
