# Changelog

All notable changes to LAIN are documented here. Versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **Intent & observability layer.** Two new MCP tools: `lain_intent` (declare
  or update per-agent goals and scopes) and `list_active_intents` (per-agent
  activity feed). Added `POST /hook` for observation ingestion and
  `POST /hook/evaluate` for pre-edit consults. Three-level coordination engine
  (GREEN / YELLOW / RED) incorporates static graph-distance refinement over
  `Calls` edges. Added `unregister_agent` tool and extended state persistence
  for intents and activities. Claims fail closed on lock timeout and survive
  server restarts without stale owner leases. `lain setup --agent claude`
  writes a startup system prompt to `.lain/PROMPT.md`.
- **LSP cold-boot prewarm.** Warms up language servers before indexing to
  prevent circuit-breaker timeouts. Configurable via `lsp_prewarm_timeout_secs`,
  `lsp_prewarm_max_files`, `lsp_prewarm_opt_out`, and per-language opt-out
  `lsp_prewarm_skip_extensions`. Warm-up status is exposed in `GET /health`
  and `lain doctor --json`.
- **Batched `install_language_server` with auto-detect.** Supports an
  `extensions` array and an `"auto"` detector that discovers languages used by
  tracked files in the workspace, returning structured per-extension outcomes.
- **Semantic-default tool profile.** `tools/list` returns a curated 14-tool
  semantic surface by default. Full surface remains accessible via
  `LAIN_TOOL_PROFILE=full` and active profile is reported in `get_capabilities`.
- **Dynamic-dispatch mitigation.** Heuristic static sensors detect message
  buses, DI containers, router decorators, Rust trait objects (`dyn Trait`),
  async task spawns, dynamic eval (`eval`, `exec`, `Function`), and TypeScript
  type escapes (`as any`). Added `explain_dispatch` tool to diagnose dispatch
  paths. `assess_change` includes heuristic callers (`include_weak_edges=true`)
  and surfaces `risk=low* — heuristic-only` when static callers are absent.
  Added `NodeType::Synthetic` for hub nodes and precompiled regexes in
  `dynamic_dispatch_sensor`. Added `lain hooks backfill-heuristics` CLI.
- **Runtime trace / OTLP HTTP ingest.** Minimal OTLP HTTP/JSON adapter and
  listener at `/v1/traces` (enabled via `LAIN_TRACE_RUNTIME=true`) that mints
  runtime `RuntimeCall` edges. Added federation-aware OTLP resolver honoring
  `code.repo` and `service.name` attributes.
- **Sidecar Git sensor.** Bundled `lain-git-sidecar` child daemon with protocol
  versioning, handshake, watchdog monitoring, and supervisor lifecycle.
- **Contextual annotations.** `explain_symbol` and `get_blast_radius` append an
  `### Open annotations` section when target symbols have matching entries.

### Fixed

- **Relative workspace paths in `repos.yaml`.** Prevented silent 0-file indexing
  when workspace paths are relative.
- **HTTP listener startup ordering.** `TcpListener::bind` runs before spawning
  the background startup re-index task so the port binds immediately.
- **Federation cold-start loop.** Cycle-detection guard suppresses repeated
  `index_forced` self-trigger loops during cold-start.
- **Coordination lock race.** `with_shared_presence` fails closed on lock
  timeout instead of proceeding unlocked.

### Removed

- Cleaned up obsolete build artifacts and temporary files.

## [0.7.4] — 2026-09-16

### Added

- **Cosign keyless signing.** Platform release tarballs include
  `<tarball>.cosign.bundle.json` signed via GitHub Actions OIDC identity.
- **Dev SPA override.** `LAIN_DEV_SPA_DIR` allows serving Command Center assets
  from disk without rebuilding.
- **Recorder CLI flag.** Added `--ready-timeout-ms` flag to the SPA demo recorder.

### Removed

- **Deleted unverified releases.** Removed `v0.7.0`–`v0.7.3` GitHub release
  assets that predated provenance, SBOM, and checksum signing (tags preserved).

### Fixed

- **Cross-platform path normalization.** MCP tool responses and `AuditEvent.path`
  consistently serialize paths with forward slashes across all platforms.
- **Release packaging alignment.** Updated `Formula/lain.rb` asset URLs and
  sha256 sums to match published release assets, and marked `v0.7.4-rc1` as
  prerelease in GitHub Releases.
- **Bounded MCP request body.** Capped `/mcp` POST body at 4 MiB, returning HTTP
  413 for oversized payloads.
- **Workspace discovery in tests.** Avoids parent-process walk-up hijacking when
  tests run inside `cargo test` / `cargo run`.
- **Test robustness and leak fixes.** Fixed temporary file leaks in e2e scripts,
  made `canonical_claim_path` public for fuzz tests, and prevented fork-PR 403s
  in CI.
- **Cold-boot race.** Resolved "Node not found for handle" startup race via
  `RepoIndex::indexed_signal` and bounded wait.
- **Hot-reload starvation mitigation.** Added cooperative yield in hot-reload
  stress tests on macOS.

## [0.6.2] — 2026-08-28

### Fixed

- **Documentation drift.** Aligned documented MCP tool counts and examples with
  the canonical 67-tool surface.
- **Tool argument naming.** Renamed `get_repo_info` parameter `id` to `repo_id`
  for consistency across federation tools.
- **Demo binary freshness.** `scripts/demo.sh` verifies that the binary is newer
  than source files, adding `--force-build` and `--allow-stale` flags.

## [0.6.1] — 2026-08-28

### Added

- **Federation per-repository readiness.** Exposed per-repository readiness
  and staleness metrics via `PerRepoReadiness` and `get_capabilities`.
- **Agent annotations & handoff layer.** Added SQLite-backed MCP tools
  `add_annotation`, `list_annotations`, `resolve_annotation`,
  `leave_handoff_note`, and `get_pending_handoffs`.

### Changed

- **Enriched health badge comment.** PR comment includes capability readiness,
  open annotations, and previous-run deltas.

### Fixed

- **Kimi parent-process discovery.** `lain mcp` resolves workspace by reading
  `/proc/$PPID/cwd` on Linux.
- **Strict workspace resolution.** `resolve_workspaces_strict` provides clear
  remediation errors when no workspace is found, and auto-delegates
  multi-workspace setups to federation mode.
- **Plugin wrapper flag order.** Corrected `--workspace` argument placement in
  `kimi_plugin_wrapper.sh`.

## [0.6.0] — 2026-08-20

Initial public release. Federation `lain server` + single-repo `lain mcp` +
Command Center SPA.
