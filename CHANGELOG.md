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
- **`Formula/lain.rb` v0.7.4-rc1 download URLs and sha256 sums now
  match the actual published release.** The formula declared
  `version "0.7.4-rc1"` but its `url`/`sha256` lines still pointed
  at the `v0.7.3` tarballs — meaning every supported Homebrew
  install fetched the old binary and the formula's own
  `assert_match "lain 0.7.4-rc1"` test would fail against it.
  Updated all three platform blocks (macOS arm64, Linux x86_64,
  Windows x86_64) to point at the real `v0.7.4-rc1` assets on the
  GitHub release. Hashes pulled from the per-asset `.sha256` files
  uploaded alongside the binaries.
- **v0.7.4-rc1 GitHub release is now flagged as `prerelease: true`.**
  The release was published without the pre-release flag set, so
  npm's `latest` dist-tag handling didn't differentiate it from a
  final release. Marked it via the GitHub releases API so any
  consumer keying off the flag (npm `next` vs `latest`, downstream
  tooling that hides pre-releases, etc.) gets the right signal.
- **`/mcp` HTTP body is now bounded at 4 MiB.** The handler used to
  do `req.collect().await?` with no size cap, so a single oversized
  POST could exhaust server memory. Added a Content-Length
  precheck (returns 413 immediately if the header advertises a body
  > 4 MiB) and wrapped the stream in `http_body_util::Limited` so
  chunked-encoded bodies without Content-Length hit the same 413
  cap. The 4 MiB cap is generous for any legitimate MCP tool-call
  payload we accept.
- **`tests/e2e/federation_dashboard_e2e.sh` and
  `tests/e2e/multiplayer-hooks.sh` no longer leak response files
  on failure.** Both scripts used `mktemp` (no `-p`), so the
  response file landed in `/tmp` and survived any non-zero exit
  before the explicit `rm -f "$TMP"`. Switched to
  `mktemp -p "${WORKDIR}"` (resp. `$TMPDIR`) so the existing
  EXIT trap's `rm -rf` catches them on every exit path.
- **`fuzz/fuzz_targets/path_canonicalize.rs` now compiles.**
  `canonical_claim_path` in `src/server/presence.rs` was declared
  `fn` (crate-private) so the fuzz target's
  `use lain::server::presence::canonical_claim_path;` failed.
  Made it `pub`; the function is documented and used by 4
  internal call sites, so exposing it as part of the public API
  surface is intentional.
- **`agent-contract` CI job now skips cleanly on fork PRs.**
  GitHub downgrades `GITHUB_TOKEN` to read-only for
  `pull_request` events from forks, so the job's
  `gh api ... statuses/...` POST would 403 every fork-PR run.
  Added a fork guard to the `if:` (plus a comment pointing at
  the separate cross-repo status publishing problem for the
  branch-protection check itself).
- **`ci.yml` version-drift extractor now accepts pre-release
  tags.** The regex `v[0-9]+\.[0-9]+\.[0-9]+` would silently
  produce an empty match for `v0.7.4-rc1` (or any future
  `-rcN`/`-beta.N`). Extended to
  `v[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?` so the same extractor
  keeps working across stable and pre-release tags.
- **`npm-shim/package-lock.json` regenerated to match `package.json`.**
  The package metadata was at `0.7.4-rc1` but the lockfile's
  top-level `version` + `packages[""].version` were still
  `0.7.3`. Ran `npm install` in `npm-shim/`; no transitive deps
  changed (the package has no production deps), only the
  metadata aligned.
- **`docs/BRANCHING.md`, `docs/CII_OWNER_ATTESTATIONS.md`,
  `docs/SUPPLY_CHAIN.md`, and `docs/VULNS.md` refreshed.** Four
  stale claims surfaced by automated review on this PR:
  the BRANCHING `if: ${{ env.full-battery }}` line that was
  actually inline `github.ref`/`github.base_ref`; the CII
  attestation's false "default crypto provider aws-lc-rs"
  claim (Cargo.toml still uses reqwest's `rustls-tls` feature
  which pulls ring) and "all inputs are bounded" claim (now
  true with the `/mcp` cap above); the SUPPLY_CHAIN example
  that claimed `version-check` exposes an `epoch` output (it
  doesn't — that path was silently dropped, see release.yml
  for the actual per-build-job local computation); and
  VULNS.md's bucket-D which still listed `bincode@1.3.3` as
  unfixed when PR #57 had already migrated it to 2.0.x.
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
- **Cold-boot "Node not found for handle" race in
  `feat_negative_paths_end_to_end` — promoted from flake to hard
  gate.** The symptom was a ~20–25% intermittent failure on
  `feat_negative_paths_end_to_end` that surfaced two bugs stacked
  on top of each other:
  1. *Root cause (test fixture):* `tests/feat_negative_paths.rs::
     boot_server` declared three `tempfile::TempDir` values as
     locals; on return, `Drop` ran `remove_dir_all` while the
     spawned `lain server` child was still serving requests. The
     watcher's first `index_forced` then re-walked an empty
     `get_all_tracked_files()` and `prune_orphans` wiped the
     per-repo graph. Switched to `TempDir::keep()` so the dirs
     outlive the fixture.
  2. *Real but secondary cold-boot race (library code):* the
     per-repo `RepoIndex` and the federation backend did not share
     an "indexed" signal, so a tool call landing in the cold-boot
     window could see an empty per-repo graph even after
     `index()` returned. Added `RepoIndex::indexed_signal` (a
     `tokio::sync::Notify` fired after every successful
     `index()` / `index_forced()`) and a 200 ms bounded wait in the
     MCP dispatcher when `ctx.graph` is empty, with a fail-through
     to the existing federation fallback. Also switched
     `tests/common/mod.rs::wait_for_repo_index` from
     `tools_call_text` to `tools_call_envelope` so it can poll
     through the cold-boot window instead of panicking on
     `isError=true`.
  Reliability: `feat_negative_paths_end_to_end` was ~75% baseline,
  25/25 after both fixes (verified locally on commit `3436a51`).
  `feat_negative_paths_end_to_end` is now treated as a hard-gate
  test in CI — no special `#[ignore]` or runner-level tolerance.

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

- **Federation per-repository readiness aggregation (M4 step 8).**
  New `PerRepoReadiness` DTO and `FederatedIndex::per_repo_readiness()`
  snapshot expose every repo's `state`, `indexed_signal`,
  `last_indexed_commit`, `last_indexed_at_unix_ms`, `outstanding_files`,
  and `staleness` (mapping `RepoHealth` to the existing
  `CapabilityState`). `get_capabilities` now includes these deep
  per-repo fields alongside the existing `repositories[].capabilities`
  shape; the aggregate `capabilities` and `SchemaVersion` are
  unchanged so old clients keep parsing. Federation-aggregate tools
  (`search_org`, `get_cross_repo_blast_radius`) were already gated
  via the central `gate_federated_tool_call`; this commit layers the
  snapshot path and the per-repo wire shape. New
  `tests/federation_readiness.rs` (7 tests) pins the contract.
- **Agent-side annotation + handoff layer (5 new MCP tools).** New
  per-repo SQLite storage at `<state_dir>/annotations/<repo>.sqlite`
  backing `add_annotation`, `list_annotations`, `resolve_annotation`,
  `leave_handoff_note`, and `get_pending_handoffs`. Live-staleness
  pass on `list_annotations` re-checks each open row's target
  against the live graph and marks rows with missing targets as
  `status: "stale"`. Schema dump regen (via `cargo run -- schema
  dump`) advertises the 5 new entries; diff is exactly the new
  tools. `LainServer` gains `annotations: Arc<AnnotationRegistry>`
  + `annotations()` accessor + `federation_repos()` helper.
- `cli::workspace::parent_process_cwd()` and a new
  `find_git_workspace_root_resolved()` policy that prefers the parent
  cwd over the process cwd. `find_git_workspace_root()` is the public
  wrapper that wires this in; the existing `Some(p)` / `None` ergonomics
  are preserved.

### Changed

- **`lain-health-badge` PR comment is enriched.** The sticky comment
  now leads with a `Capability readiness: ...` line from
  `get_capabilities`, lists open annotations per file (first 3
  rows + a "more..." link to the underlying `list_annotations` MCP
  call), and adds a "Previous-run delta" section that calls
  `explain_symbol` for every modified (not just added) function in
  the PR against the base ref's previous commit. All three
  additions are best-effort — a failed MCP call must not fail the
  badge itself. No new `action.yml` inputs.

### Fixed

- **`tests/feat_negative_paths.rs` baseline compile error.** The
  recent merge to `dev` (4c885c3) added `.keep()` calls that
  consumed `TempDir`s but the function's return type still
  expected `TempDir`. Replaced with `.path().to_path_buf()` so the
  same `PathBuf` is derived without moving the `TempDir`. Without
  this fix `cargo build --workspace --all-targets` failed on the
  branch baseline.

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
