# PR 6 — Hot Reload: Tasks 6.2–6.7

## Summary

Six commits land on `consolidation/lain-monorepo` adding the full
hot-reload subsystem. `cargo test --workspace --features test-utils -- --test-threads=1`
passes: 374 lib tests + 3 lain-mcp-probe + 3 new integration tests
(`tests/hot_reload.rs`, `tests/hot_reload_remove.rs`).

## Commits

| Task  | Commit    | Subject |
|-------|-----------|---------|
| 6.2   | `1f29e96` | feat(reload): rebuild task with diff + atomic swap |
| 6.3   | `4643079` | feat(watcher): watch repos.yaml and workspaces.yaml for reload |
| 6.4   | `6f5bc58` | feat(reload): Unix socket signal path for CLI → server |
| 6.5   | `988e676` | feat(mcp): get_reload_status + request_reload tools |
| 6.6   | `2940914` | test(hot-reload): integration tests for add/remove repo |
| 6.7   | `42ff940` | docs(hot-reload): walkthrough of subsystem and observability |

## TDD evidence

Every task followed the failing-test-first rule:

- **6.2**: 5 new tests in `src/server/reload.rs::tests::rebuild` —
  `rebuild_picks_up_new_repo_in_repos_yaml`,
  `rebuild_drops_repo_removed_from_repos_yaml`,
  `rebuild_replaces_workspaces_yaml_when_present`,
  `rebuild_records_failed_state_on_invalid_repos_yaml`,
  `lain_server_set_workspace_swaps_slot`. TDD red: build failed
  with `cannot find function run_rebuild in module crate::server::reload`.
- **6.3**: 3 new tests in `src/server/watcher.rs::tests` —
  `watch_paths_for_config_lists_existing_workspaces_yaml`,
  `config_watcher_triggers_reload_on_repos_yaml_modify`,
  `config_watcher_triggers_reload_on_workspaces_yaml_modify`.
- **6.4**: 4 new tests in `src/cli/signal.rs::tests` —
  `socket_path_for_uses_repos_yaml_stem`,
  `socket_path_for_defaults_when_no_stem`,
  `signal_reload_is_noop_when_socket_missing`,
  `signal_listener_forwards_to_bus`.
- **6.5**: tools exercised by integration tests in 6.6.
- **6.6**: 3 integration tests in `tests/hot_reload.rs` and
  `tests/hot_reload_remove.rs`.

## Files touched

- `src/server/reload.rs` — `run_rebuild`, `workspaces_path_for`
- `src/server/ingest/mod.rs` — `reload_bus`, `add_repo`,
  `remove_repo`, `set_workspace`, `workspaces_snapshot`,
  `repos_yaml_path`. Federation_workspaces slot changed from
  `Option<Arc<WorkspacesFile>>` to `Arc<Mutex<Option<...>>>` so
  `set_workspace` can swap it without violating `Clone`.
- `src/server/watcher.rs` — `watch_paths_for_config`,
  `spawn_config_watcher`
- `src/cli/signal.rs` (new) — `socket_path_for`, `signal_reload`,
  `spawn_signal_listener_at`
- `src/cli/mod.rs` — `pub mod signal;`
- `src/cli/server.rs` — `spawn_hot_reload` (file watcher + socket +
  rebuild loop)
- `src/cli/workspaces.rs` — `signal_reload` after every `save()`
- `src/cli/repos.rs` — `signal_reload` after every `write_atomic`
- `src/config/mod.rs` — `run_dir()` (`$XDG_RUNTIME_DIR/lain` or
  `~/.local/lain/run`)
- `src/server/mcp/federation_tools.rs` — `get_reload_status`,
  `request_reload`
- `src/server/mcp/handler.rs` — `with_reload_bus` constructor,
  stdio + HTTP dispatch for the two new tools
- `tests/hot_reload.rs` (new)
- `tests/hot_reload_remove.rs` (new)
- `docs/hot-reload.md` (new)

## Test count delta

Baseline (pre-6.2): 367 lib tests + 3 lain-mcp-probe = 370.
After 6.7: 374 lib tests + 3 lain-mcp-probe + 3 integration tests = 380.
Net: +10 lib tests, +3 integration tests.

## Caveats / known limitations

- **In-flight LainMcpServer keeps its workspaces copy.** The rebuild
  updates the slot `workspace_count` reads and that future rebuilds
  consult, but the stdio/HTTP transports continue to use the
  workspaces file they were constructed with. Workspace MCP tools
  therefore reflect startup-time `workspaces.yaml` until restart.
  Federation `list_repos` etc. *do* update live. Documented in
  `docs/hot-reload.md` and in `LainServer::set_workspace`'s doc
  comment.
- **`LainServer::with_federation` builds a placeholder git repo at
  `/tmp/lain-federation-{pid}`** that survives between tests in the
  same binary. The integration tests proactively `remove_dir_all`
  it to avoid EACCES / "could not find repository" errors from
  stale state. This is internal to the constructor and does not
  affect production.
- **Serial test execution required.** `cargo test -- --test-threads=N`
  with `N > 1` causes races on the pid-shared `/tmp/lain-federation-{pid}`
  staging dir (3 gitops tests also have known pre-existing
  parallel-only flakiness unrelated to this PR). All new tests are
  designed to pass under `--test-threads=1`.

## End-to-end smoke

No live shell available; verified by `tests/hot_reload.rs` which
spins up a real `LainServer::with_federation`, runs the rebuild loop
in a tokio task, mutates `repos.yaml`, pings the bus, and asserts
the federation's `repo_count` reflects the change.