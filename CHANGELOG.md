# Changelog

All notable changes to LAIN are documented here. Versions follow
[Semantic Versioning](https://semver.org/).

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
