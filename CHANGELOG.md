# Changelog

All notable changes to LAIN are documented here. Versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- **Windows path-format normalization.** All MCP tool responses that
  surface a file path (`claim_files`, `release_files`, `list_occupancy`,
  `my_claims`, `list_my_claims`, `detect_overlap`) now serialize paths
  in forward-slash form on every platform. Previously Windows clients
  received `"src\\a.rs"` where the wire contract required
  `"src/a.rs"`, breaking the contract and the `multi_agent_concurrency`
  integration tests. A new `crate::server::path_util::posix_string`
  helper is the canonical cross-platform path string renderer,
  mirroring the existing `graph_path` pattern.
- **Audit log JSONL is now platform-independent.** `AuditEvent.path`
  is written in forward-slash form regardless of host OS, so the
  `get_recent_activity` `path_glob` filter (built with `/`) matches on
  Windows as it does on Linux. The same fix applies to the
  `group_by: "path"` branch of `group_key` in `audit_tools.rs`. No
  in-process reader other than `read_audit_log` consumes the JSONL
  today, so the wire-format change is internal.
- **Removed two `#[cfg_attr(target_os = "windows", ignore)]` gates**
  on `claim_files_accepts_string_form_files` and
  `get_recent_activity_tool_groups_by_path` in `tests/presence.rs`.
  Both tests now run on Windows after the underlying fixes.
- **`tests/multi_agent_concurrency.rs`** replaces eleven raw
  `Some("src/...")` literal assertions with a local
  `path_components_eq` helper, matching the existing helper in
  `tests/feat_suite.rs`. These tests now run unmodified on every
  platform.
- **Workspace discovery no longer hijacks on dev/test runs.** When
  the parent process is `cargo test` / `cargo run`, its cwd is the
  project containing the `lain` binary itself, so the
  parent-process-cwd walk-up used to land on the source tree and
  `lain mcp` was asked to re-index the entire project being tested
  — the `oneshot_discovers_workspace_from_cwd` regression test
  timed out at 60 s. `find_git_workspace_root_resolved` now skips
  the parent-cwd candidate when the running binary lives inside
  the git root it resolved to, falling through to the process's
  own cwd. Real agent harnesses (Kimi, Claude Code, a plain
  shell) put the binary in a plugin dir or on `$PATH`, so the
  filter never fires for them.
- **Recorder `--ready-timeout-ms` flag.** The SPA recorder
  (`tests/js/record_spa_demo.js`) previously hard-coded a 600_000
  ms cap on its `waitForReady` poll. Cold-cache CI hosts occasionally
  exceeded that; the only escape was editing the source. The flag
  is now a CLI arg (default unchanged at 600_000). A regression
  test (`tests/js/recorder_cli.test.js`) pins the parser shape.
- **Dev SPA override via `LAIN_DEV_SPA_DIR`.** The Command Center
  SPA was `include_bytes!`'d at compile time, so every JS/CSS edit
  required `cargo build`. Setting `LAIN_DEV_SPA_DIR=<path>` now
  flips the assets module to read each file from disk on demand
  (one env-var lookup + one `is_dir` check per request). Edit
  `app.js` / `styles.css` / `index.html`, save, refresh the
  browser — no rebuild. Production builds leave the env var unset
  and the contract tests still pass. Workflow script:
  `scripts/dev-spa.sh`.
- **Hot-reload writer starvation on macOS — speculative `yield_now`
  mitigation.** `set_workspace_stress_visible_to_shared_lock`
  in `tests/hot_reload.rs` was gated on macOS because the reader
  occasionally collapsed to a single distinct count. The writer
  task did 100 synchronous `set_workspace` calls with no
  `.await` between them; on macOS's kqueue-based scheduler the
  writer monopolized a worker thread for the burst and the reader
  woke up only after the writer finished. Inserting
  `tokio::task::yield_now().await` between writes closes the
  starvation window on every platform. The test still passes on
  Linux (behavior-neutral change); the macOS gate stays until a
  real macOS runner confirms the mitigation removes the flake.

### Investigated (no change)

- **CI `cargo build --bin lain` step is correctly unconditional.**
  The parked-bug inventory note flagged this step as a candidate
  for an `if: matrix.os == 'windows-latest'` guard. It cannot —
  the step exists because `tests/use_cases/battery_*` (specifically
  `battery_cli.rs` and `battery_success_metrics.rs`) invoke the
  binary as a subprocess, and those tests run on all three OS
  matrices (Linux, macOS, Windows), not just Windows. No code
  change; the workflow is left as-is.
- **macOS FSEvents config-watcher latency — documented as resolved by gate.**
  `config_watcher_triggers_reload_on_repos_yaml_modify` and
  `config_watcher_triggers_reload_on_workspaces_yaml_modify` were
  gated with `#[cfg_attr(target_os = "macos", ignore)]` because
  FSEvents coalescing latency is unbounded within any reasonable
  CI budget. The project's CI saga documented the iteration:
  5 s → 15 s → 30 s → gate (commits `0a341b5` → `6adc721` →
  `e29c4dc` → `7af23fd`). The only durable Linux-untested
  alternative — switching to `notify::PollWatcher` on macOS — is
  out of scope for this plan because it requires macOS hardware
  to verify. The gates stay; the rationale is documented for the
  next reader.

## [0.6.2] — 2026-08-28

### Fixed

- **Docs sweep — close 13 drift rows from the audit.** `docs/quickstart-tools.md`
  drops the non-existent `export_graph_json` heading (was 270) and adds
  a *Tools documented elsewhere* section linking to the 28 tools that
  live in `FEDERATION.md` / `multiplayer.md` / `hot-reload.md` /
  `command-center.md`; the canonical surface is 67 tools, so this page
  now covers 39 of them, not all of them. `docs/TECHNICAL.md` swaps a
  `curl … export_graph_json` example for `describe_schema`, and notes
  that `lain schema dump` is the wire-format authority. `docs/ARCHITECTURE.md`
  swaps the "1500 lines" guesstimate for "~1230 lines" (`app.js` is
  1234). `docs/multiplayer.md` corrects the multiplayer tool count from
  "8 new MCP tools" to 14 (8 inline + the 5 listed in their sections)
  and disambiguates the `world_state` envelope field from the
  `get_world_state` MCP tool. `docs/USER_MANUAL.md` and `docs/quickstart-tools.md`
  clarify that `semantic_search` is *filtered from `tools/list`* when no
  NLP model is loaded (66 of 67 advertised), not advertised with an
  "unavailable" answer. `README.md` flips the mermaid agent label from
  Cursor to Agy/Codex (Cursor has no full hook script in `hooks/`) and
  rewords the `lain ask` row. `docs/hot-reload.md` polls every 2 s (was
  every second). `docs/wish-list.md` refreshes the `61 / 63 / 64` tool
  counts to match the canonical 67. `index.html`/`theme.css`/etc. were
  not touched; no code changes shipped.

- **D-H3 tool-arg consistency.** The `get_repo_info` MCP tool's required
  argument is renamed from `id` to `repo_id`. The old name was confusing
  alongside sibling tools that use `agent_id` / `session_token`, and
  already aligned with `get_cross_repo_blast_radius_for_repo`. Callers
  must update their request bodies; the `docs/FEDERATION.md` reference
  page and `scripts/demo.sh` are updated alongside, and a
  `tool_args_for_caller_identity_are_named_consistently` regression test
  pins the surface so the next drift fails loudly.

- **D-L3 demo.sh binary freshness.** `scripts/demo.sh` now prints the
  binary's version *and* mtime on startup, and warns and exits 2 when
  any source file (`Cargo.toml`, `Cargo.lock`, `src/**/*.rs`) is newer
  than the binary. Previously `--quick` and `--no-build` skipped the
  build but still ran `target/release/lain`, so a demo could silently
  measure a stale binary and report it as current. New flags:
  `--force-build` (rebuild even under `--quick` / `--no-build`) and
  `--allow-stale` (skip the check). The comparison lives in a sourced
  helper, `scripts/demo-freshness.sh`, covered by
  `tests/demo_sh_freshness.sh`.

## [0.6.1] — 2026-08-28

### Fixed

- **Kimi integration (the headline fix).** `lain mcp` now reads the
  parent agent's cwd via `/proc/$PPID/cwd` on Linux and walks up for
  `.git` from there, falling back to the process's own cwd. Kimi's
  plugin security model pins the MCP subprocess cwd to the plugin
  root, so under 0.6.0 a naive `{"command":"lain","args":["mcp"]}`
  config resolved to the plugin directory instead of the project.
  With 0.6.1 the same config works under Kimi without any wrapper
  script. macOS is unsupported in either path. (Linux only.)

- **`src/cli/kimi_plugin_wrapper.sh`** rewritten to insert
  `--workspace <git_root>` *after* the `mcp` subcommand, because
  clap parses `--workspace` as a flag on `mcp`, not on the top-level
  binary. The earlier sentinel-rewrite form produced
  `lain --workspace <path> mcp`, which clap rejected with
  `unexpected argument '--workspace' found`. The wrapper is no longer
  required for Kimi; it remains in source as a fallback for users
  pinned to the 0.6.0 binary.

### Added

- `cli::workspace::parent_process_cwd()` and a new
  `find_git_workspace_root_resolved()` policy that prefers the parent
  cwd over the process cwd. `find_git_workspace_root()` is the public
  wrapper that wires this in; the existing `Some(p)` / `None` ergonomics
  are preserved.

- `cli::mcp::resolve_workspaces()` and a strict variant that errors
  when no workspace can be resolved. `resolve_workspaces_strict()`
  backs `run_mcp` so a Kimi-style cwd-pinned spawn fails fast with a
  message that names the four ways to fix it (`--workspace PATH`,
  `LAIN_WORKSPACE`, run inside a clone, or pass the wrapper script
  on 0.6.0).

- Multi-workspace delegation: when `resolve_workspaces_strict()` finds
  more than one workspace, `run_mcp` synthesizes a temp `repos.yaml`
  and delegates to `run_server --transport stdio`, giving the agent
  the same federation surface as `lain server` without having to
  author the config itself.

### Docs

- README: explicit Kimi note explaining the native `/proc/$PPID/cwd`
  path on 0.6.1 and the wrapper as a 0.6.0 fallback.
- README + `docs/command-center.md`: chromium-captured Command Center
  screenshots on the Overview / Repos / Tools tabs.
- `docs/TECHNICAL.md`: workspace-resolution policy documented as a
  numbered list, matching the new `cli::mcp::resolve_workspaces`
  order.

### Verified

- `cargo test --release`: 41 test binaries, ~970 tests passing,
  0 failed, 2 ignored (semantic_search path: ONNX model not loaded).
- `scripts/demo.sh --quick`: 111/111 ground-truth fixture assertions
  pass.
- Federation smoke test against three real repos on disk
  (`pii-sentinel`, `free-pmo`, `qap-metaheuristics`): 63 tools,
  3/3 ready, `find_anchors repo_id=pii-sentinel` returns 5 real
  anchors.

## [0.6.0] — 2026-08-20

Initial public release. Federation `lain server` + single-repo
`lain mcp` + Command Center SPA. See `README.md` and the docs
index in `docs/INDEX.md`.
