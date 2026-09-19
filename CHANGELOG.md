# Changelog

All notable changes to LAIN are documented here. Versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **LSP cold-boot prewarm.** On every server start, each language
  server the workspace actually uses gets one warm-up
  `documentSymbol` call against a sentinel file (largest by
  mtime, bounded by `lsp_prewarm_max_files`) before `build_core_memory`
  reaches the scan batch. Cold-cache `rust-analyzer` / `clangd` no
  longer trip the runtime 1 s circuit breaker on the first real
  call (introduced in #112), so they stay out of tree-sitter-only
  fallback. New `IngestionConfig` knobs: `lsp_prewarm_timeout_secs`
  (default 30), `lsp_prewarm_max_files` (50), `lsp_prewarm_opt_out`
  (false). Readiness.phase advances through a new `PrewarmingLsp`
  state so operators see warm-up progress via `get_capabilities`
  and the Command Center. The prewarm path is fully isolated from
  the runtime circuit breaker — a slow / failing prewarm never
  marks a binary `unavailable`. Per-language opt-out via
  `lsp_prewarm_skip_extensions` (Vec<String>; default empty) for
  monorepos that mix languages whose LSPs are intentionally
  unavailable.

- **Batched `install_language_server` with auto-detect.** The
  existing single-install tool now accepts a batched `extensions`
  array, plus a special `"auto"` entry that expands to every
  language the workspace's tracked files use. Response carries a
  per-extension outcome (`installed`, `already_installed`,
  `unknown_ext`, `no_install_cmd`, `failed`) so an agent can
  decide whether to retry without parsing free-text errors.
  Idempotent: any binary already on PATH reports
  `already_installed` without spawning a duplicate install
  command. Backward compatible — passing `{language: "rust"}`
  works exactly as before. The `"auto"` entry now returns a
  typed `LainError::Config` instead of silently empty when the
  workspace isn't a git repository.

- **Semantic-default tool profile.** `tools/list` now returns the
  curated 14-tool semantic surface by default — the M5 bootstrap
  (`understand_repository`), the M6 high-level Agent API
  (`find_symbol`, `get_context`, `find_related`, `assess_change`,
  `search_code`), readiness + self-discovery (`get_health`,
  `get_capabilities`), and the multiplayer essentials
  (`register_agent`, `heartbeat`, `claim_files`, `release_files`,
  `get_world_state`) — with `get_agent_strategy` kept as an
  on-demand escape hatch to the full list. Federation, workspace,
  and server-status families are visible when in their respective
  modes. The legacy 79-tool surface is reachable via
  `LAIN_TOOL_PROFILE=full`. Active profile is exposed through
  `get_capabilities.tool_profile` so agents self-discover which
  filter is in effect at startup. The on-disk
  `docs/tool-schema.json` is unchanged: schema-drift CI still
  validates the fully-populated shape, only the runtime wire
  shrinks.

- **LSP prewarm visibility + operator knobs.** `GET /health` now
  includes `lsp_prewarm: { binary: { status, ms?, reason? } }` —
  one entry per LSP binary that ran warm-up, with status
  `warmed` / `timed_out` / `failed` / `skipped_no_sentinel` /
  `skipped_unavailable` (snake_case). `lain doctor --json` reports
  the active `tool_profile.{name, advertised_count}` and the
  resolved `lsp_prewarm.{timeout_secs, max_files, opt_out,
  env_opt_out}`. Federation and workspace modes are detected from
  disk (`repos.yaml` multi-repo / `workspaces.yaml` present), so
  the offline `doctor.json` snapshot matches what the live server
  reports on `tools/list`. Workspace-aware advertised_count is
  exact under all modes: the workspace handle is plumbed from
  `LainMcpServer` into `ToolContext` so `get_capabilities`
  reflects the live wire; doctor reads `workspaces.yaml` from disk
  to compute the same count offline.

- **README TL;DR + quickstart-tools callouts.** README's "See
  it run" lists cold-boot prewarm; "What can AI Agents ask LAIN?"
  gained sections 7 (prewarm) and 8 (curated profile); "TL;DR —
  Install in 30 Seconds" gained an `Operator knobs` table covering
  `LAIN_TOOL_PROFILE`, `LAIN_LSP_PREWARM`, and the three prewarm
  tunables with `lain doctor --json` as the diagnostic surface.
  `docs/quickstart-tools.md` was updated alongside.

- **Dynamic-dispatch mitigation (three tiers).** Tree-sitter + LSP
  cannot follow dynamic dispatch (message buses, DI containers,
  schema-driven routers, reflection). The static graph returned
  empty `Calls`/`Uses` lists for any caller routed through those
  patterns, and `get_blast_radius` reported "no dependents found"
  even when the application depended on the function. Three new
  layers close the gap:

  - **Tier 1 — protocol composition.** `get_agent_strategy` now
    teaches the agent to compose `get_blast_radius` with
    `get_coupling_radar`, `find_anchors`, `trace_dependency`, and
    `explain_dispatch` rather than trust a single tool. The
    pre-commit hook (`hooks/claude-code/pre-commit.sh`) now runs
    `LAIN_SMOKE_CMD` (with `LAIN_SMOKE_TIMEOUT_SECS`) when set,
    so the smoke gate is the catch-all for changes the static
    graph cannot reason about. New `docs/dynamic-boundaries.md`
    template for per-repo registration of message buses, DI
    containers, and routers.

  - **Tier 2 — heuristic edges.** New `EdgeType` variants
    `DynamicDispatch`, `BusTopic`, `RouteMatches`. New
    `EdgeProvenance { Static, Heuristic { detector, confidence },
    Runtime { trace_id, last_seen_unix } }` carried on every
    `GraphEdge`. New `dynamic_dispatch_sensor` walks the
    workspace and emits heuristic edges from matching files to
    deterministic `Hub:<detector>` synthetic nodes, with per-
    detector confidence constants (message_bus=0.7,
    container=0.6, schema_router=0.5, reflection=0.4).
    `get_blast_radius` gained `include_weak_edges` and honours
    `LAIN_HEURISTIC_MIN_CONFIDENCE` (default 0.5). Below-threshold
    edges stay out of the default view.

  - **Tier 3 — runtime synthesis.** New `explain_dispatch` tool
    (in the curated 15-tool `Semantic` profile) returns
    `{verdict, static_callers, heuristic_callers, runtime_callers,
    co_change_partners}` per symbol. The `verdict` field
    (`static_only`, `heuristic_only`, `runtime_only`,
    `runtime_confirmed`, `insufficient_evidence`) is the single
    string an agent should route on. The `runtime_trace`
    module (`src/server/runtime_trace/`) carries the
    `RuntimeTraceStore` API surface; the OTLP gRPC adapter
    itself is deferred to a follow-up PR.

- **Backfill CLI.** `lain hooks backfill-heuristics
  --workspace <path> [--graph <path>] [--dry-run]` re-runs the
  heuristic sensor against an existing `.lain/graph.bin` without
  a full re-index of static edges. Idempotent: deterministic UUID
  v5 identifiers mean re-runs add zero new edges. Use after
  upgrading lain that predates Tier 2.

- **Awareness doc coverage.** All twelve agent awareness files
  under `hooks/` (`claude`, `claude-code`, `cursor`, `codex`,
  `cline`, `copilot`, `gemini`, `kimi`, `agy`, `windsurf`,
  `opencode`, plus `kimi/skills/lain/SKILL.md`) gained a
  "Dynamic Dispatch Caveat" section that links back to
  `docs/dynamic-dispatch.md` in the lain repo.

- **Umbrella doc.** `docs/dynamic-dispatch.md` describes the
  three tiers end-to-end, documents the new env vars
  (`LAIN_HEURISTIC_MIN_CONFIDENCE`, `LAIN_TRACE_TTL_SECS`,
  `LAIN_TRACE_MAX_EDGES`), and lists what the mitigation does
  not solve. Linked from `docs/INDEX.md`.

- **Polished.** Removed dead `_force_use` helper from
  `explain_dispatch`; tightened `NodeType` import scope;
  `resolve_node` rejects empty handles explicitly so the new
  empty-name `File` nodes (source of heuristic edges) cannot
  be silently matched by `find_node_by_name("")`.

- **OTLP listener startup hook.** `cli/server.rs` now binds the
  runtime-trace OTLP HTTP listener at startup, gated by
  `LAIN_TRACE_RUNTIME=true` and a configurable
  `LAIN_TRACE_OTLP_PORT` (default 4318, the OTLP/HTTP convention).
  The listener runs on its own `TcpListener`, completely separate
  from the MCP transport, so trace ingestion never shares a port
  with the agent-facing API and never competes for MCP request
  budget. Bind failure is downgraded to a startup warning — the
  server still comes up with tracing disabled rather than failing
  the boot — so an OTLP port collision on a host running other
  observability tooling no longer wedges the MCP entry point.

- **`get_blast_radius` empty-graph contract pinned.** A fresh,
  never-indexed graph now produces a structured `NotFound` from
  `get_blast_radius` rather than a confident "no callers" report.
  The handler walks incoming edges, so a zero-node graph would
  otherwise take the BFS loop's early-exit and report a symbol as
  "isolated" — wrong, because the symbol is missing, not
  unconnected. `resolve_node` already detects the empty-graph
  branch and points the agent at `get_health` and federation
  fallback; the new test in `tools::handlers::impact` pins that
  contract so a future refactor that drops the branch fails loud
  instead of silently regressing.

- **`explain_dispatch` NotFound contract pinned.** A missing
  symbol now surfaces as `NotFound` from `explain_dispatch`
  rather than a confident `verdict: "insufficient_evidence"`
  report. `explain_dispatch` resolves the symbol up-front via
  `resolve_node`, so a future refactor that replaces the
  early-return with a fall-through to `build_with_store` would
  emit a confident "no callers" answer for a symbol that doesn't
  exist at all. The new test in
  `tools::handlers::explain_dispatch` pins the up-front-resolve
  contract so that regression fails at `cargo test` time.

### Fixed

- **Relative `workspace_dir` paths in `repos.yaml` no longer
  produce a silent 0-file index.** Joining the workdir-relative
  `entry.path` onto a relative workspace produced paths like
  `./tauri/crates/tauri/src/lib.rs` that libgit2 could not resolve
  under the workdir and therefore treated as ignored, so the
  indexer reported zero tracked files per repo and the federation
  loaded empty graphs. `GitSensor::get_all_tracked_files` and
  `GitSensor::get_changed_files_since` now pass the workdir-
  relative path straight to `is_path_ignored`. Regression test:
  `get_all_tracked_files_works_with_relative_workspace_path`.
  Surfaces in the 2026-09-18 Tauri federation trial
  (`POSTMORTEM.md` Bug #1).

- **HTTP transport listener binds before the startup re-index is
  spawned.** `LainMcpServer::run_http` previously logged
  "Starting Lain MCP HTTP server" and then spawned the
  backgrounded startup re-index task before reaching
  `TcpListener::bind`. When the re-index hung, port 9999 never
  opened, no log line followed the "Starting" line, and clients
  got `connection refused`. The listener is now bound first and a
  "listening" log line fires immediately after `bind` returns, so
  a stuck startup task is distinguishable from a stuck bind. If
  the re-index does hang, the listener stays up and clients get
  the structured `warming_up` response from `dispatch_tool_call`;
  operators can poll `/health` to observe the stuck state. Root
  cause of the underlying hang (Bug #2) is still under
  investigation.

### Tier-3 follow-ups

- **`NodeType::Synthetic` for hub nodes.** Hub nodes (`Hub:
  message_bus_publisher` etc.) are no longer typed as `Function`:
  they aren't functions, they have no source location, and walking
  into them via `Calls`/`Uses` produced nonsense answers. The new
  variant carries `NodeType::Synthetic` in the schema, the
  `presence_tools::symbol_weight` tier (1, with containers), and
  the `describe_schema` doc surface. Agents that previously saw
  hub nodes show up in `find_anchors` now see them filtered out by
  the node-type filter `type_filter=function` — the correct
  outcome, since heuristic evidence should be opt-in via
  `get_blast_radius(..., include_weak_edges=true)` or
  `explain_dispatch`, not by reading hub nodes as if they were
  real functions.

- **`dynamic_dispatch_sensor` regex precompilation.** The hot loop
  in `scan_workspace_dispatch` now reads from a `Lazy<Vec<CompiledDetector>>`
  rather than compiling 12 regexes per file per scan. On a
  multi-thousand-file workspace, that's ~120k regex compilations
  per scan avoided. The precompile also resolves the
  `&str → EdgeType` and `&str → confidence` lookups once at static
  init, leaving the per-file hot loop with seven lines and a
  single allocation (the `GraphEdge` per match). Patterns that
  fail to compile are silently dropped — the failure mode is
  "this family never matches", which is honest and easier to spot
  in test coverage than a panic at static-init time.

- **`assess_change` defaults to `include_weak_edges=true`.** The
  pre-edit risk verdict (low/medium/high) is meaningless if it
  can't see dynamic-dispatch callers. A `bus.publish` site with
  only heuristic callers previously reported `direct=0,
  transitive=0, risk=low` and the agent would ship the regression
  `explain_dispatch` was built to prevent. With `include_weak_edges=true`,
  heuristic callers are tagged with `~` and `[heuristic,
  conf=X.XX]` (the same format `get_blast_radius` already uses)
  so agents can tell them apart from type-resolved calls.
  `explain_dispatch` remains the diagnostic follow-up when the
  verdict surfaces heuristic callers (it explains which detector
  fired and whether runtime traces confirm them).

- **Rust trait-object + async-spawn detectors.** Two new
  detectors close the Tier-3 review point on Rust coverage that
  was missed in the original sensor surface: `rust_trait_object`
  matches `Box<dyn Trait>`, `Rc<dyn Trait>`, `Arc<dyn Trait>`
  (where `Trait` is a user-defined identifier, not `Any`) at
  confidence 0.6; `async_task_spawn` matches `tokio::spawn`,
  `async_std::task::spawn`, `smol::spawn`, `executor::spawn`,
  `workers::spawn` at confidence 0.5. Negative coverage pinned:
  bare `spawn(` without a runtime namespace prefix does NOT
  match (prose noise, not a dispatch surface). The two new
  detectors together fill the gap that the original Tier-3
  surface had on Rust — every other dynamic-dispatch family the
  agent might reasonably hit now produces a heuristic edge
  with a confidence score.

- **`assess_change` regression test for the heuristic-caller
  path.** A new unit test in
  `src/server/tools/handlers/semantic.rs` builds a fixture with
  one `BusTopic` heuristic edge pointing at the symbol under
  assessment and pins the contract that the output must
  contain the heuristic marker (`[heuristic` /
  `heuristic caller(s) included`), the confidence tag
  (`conf=0.70`), and a non-`low` risk verdict. Without this
  test, a future refactor could silently re-introduce the
  false-negative iter 14 closed. The test also caught a real
  fixture bug along the way: the `File` node representing the
  call site was never `upsert_node`'d, so `insert_edges_batch`
  silently dropped the edge as orphan and `get_edges_to`
  returned `[]` — a useful confirmation that the
  silent-drop-on-orphan behaviour is itself worth pinning in
  a follow-up test.

- **`explain_dispatch` tests no longer use the global store.**
  `RuntimeTraceStore::global()` is a process-wide `OnceLock` —
  once a test ingests a span into it, that span persists for the
  lifetime of the test process. The
  `verdict_distinguishes_runtime_confirmed_from_runtime_only`
  test was the only one using the global; it now constructs a
  fresh `RuntimeTraceStore::new(StoreConfig::default())` and
  calls `build_with_store` directly. Removed the now-unused
  `reset_global_for_tests` seam (`OnceLock` has no `take()`,
  the function was a no-op, and no caller existed).

- **`dynamic_eval` detector closes the Python + JS gap.**
  `eval()`, `exec()`, `pickle.loads()`, and `new Function()` are
  direct string-to-code surfaces; anything reachable from the
  loaded string is invisible to the static graph. New detector
  matches these patterns at confidence 0.4 (matches reflection
  tier) and tags them as `DynamicDispatch`. Two regression
  tests pin the contract: a Python `pickle.loads` fixture and
  a JS `eval` fixture — so a future Python-only tuning can't
  silently miss the same risk in a JS codebase.

- **`assess_change` surfaces heuristic-only honestly.** Pre-fix:
  `assess_change` on a symbol with zero static callers and one
  heuristic caller returned `risk=low` — the agent would treat
  that as safe, exactly the regression `explain_dispatch` was
  built to prevent. Post-fix: when the static graph is empty
  but the blast-radius output carries the `heuristic caller(s)
  included` marker, `assess_change` emits `risk=low* —
  heuristic-only (see ~ N heuristic caller(s) below)` instead.
  The asterisk is the agent-visible signal that the static
  graph is empty by blind-spot, not by absence of callers.
  Risk-tier ordering (`low*` → `medium` → `high`) is preserved
  so any consumer that matched on the bare `low` word still
  works. Two regression tests pin the new contract:
  `assess_change_heuristic_only_verdict_does_not_say_low` and
  the iter-17 `assess_change_surfaces_heuristic_callers_in_risk_verdict`.

- **`assess_change` truly-empty blast radius pins `risk=low`.**
  The risk-verdict tier relies on `count_bullets` to distinguish
  a truly-empty blast radius from a populated one. The old
  implementation matched any line whose trimmed prefix was
  `- `, which counted the section header itself (`- Direct
  dependents (0):`) as a bullet — a symbol with zero callers
  was therefore classified as `direct=1 / transitive=2 /
  risk=medium`. Tightening the bullet marker to two-space
  indent matches what `get_blast_radius` actually emits for the
  `affected_names` list. The new
  `assess_change_truly_empty_blast_radius_says_low` test pins
  the third vertex of the contract: `0 static + 0 heuristic =
  bare risk=low` (no asterisk, no heuristic marker). The
  iter-21 `low*` heuristic now lives strictly in the
  `0 static + N heuristic` vertex and cannot leak into the
  truly-safe path.

- **Negative-coverage regression tests for detector regexes.**
  Three tests pin what the heuristic regexes MUST NOT match
  so a future tightening can't silently start firing on
  innocent identifiers:
  - `identifier_prefixes_with_eval_or_exec_do_not_match` —
    `evaluate_query`, `execution_time`, `exec_summary`,
    `executable_path` (all common identifier forms that
    contain the eval/exec substrings) must not fire
    `dynamic_eval`.
  - `concrete_generic_types_do_not_match_rust_trait_object`
    — `Vec<MyStruct>`, `HashMap<String, MyStruct>`,
    `Box<MyStruct>` (concrete types without `dyn`) must
    not fire `rust_trait_object`.
  - `identifier_prefixes_in_dispatch_context_do_not_match` —
    the dynamic_eval version of test 1, in bus/handler.py
    shape. The comment-stripping case (commented-out
    `bus.publish(` still matches today) is intentionally
    NOT pinned — fixing it requires a tree-sitter pre-pass
    per file and is documented in the test as a future-PR
    concern.

- **`type_escape` detector for TypeScript `as any` / `as
  unknown as any` / `<any>`.** TypeScript's type escape
  hatches are the most common JS/TS-specific
  dynamic-dispatch surface we hadn't yet covered. After any
  of these casts, the value dispatches through the JavaScript
  prototype chain at runtime — the LSP can't follow, the
  static type system has been told to look the other way.
  Pinned at confidence 0.3 (lower than `serde_value`'s 0.4
  because `as any` is often a temporary workaround rather
  than an architectural dispatch surface — the user usually
  intends to remove it eventually). Three regression tests
  pin the contract: `typescript_as_any_emits_type_escape_edge`,
  `typescript_as_unknown_as_any_matches`, and the negative
  coverage `as_something_other_than_any_does_not_match`.

- **Mixed static + heuristic risk-tier pinned.** The fourth
  vertex of the `assess_change` risk-tier contract:
  `N static + N heuristic → regular tier (medium / high)`. The
  mixed case must NOT downgrade to either tier-3 special
  verdict — the static caller's presence keeps the risk tier
  on the regular scale; the heuristic evidence augments an
  already-existing caller surface but doesn't replace the
  count-based tier. Pinned by
  `assess_change_mixed_static_and_heuristic_uses_normal_tier`:
  verdict is medium or high (not bare `low` or `low*`), and
  the heuristic evidence still surfaces in the body so the
  agent knows the runtime target has more callers than the
  static graph shows.

- **`insert_edges_batch` dropped-edge count contract pinned.**
  The function silently drops edges whose endpoints are both
  missing (or whose source is missing), and the production
  caller `insert_edges_reporting` emits a `warn!` carrying the
  count. The function returns the count but no test was
  pinning that contract — iter 17 found the silent-drop
  behaviour the hard way when an `assess_change` fixture
  forgot to upsert one endpoint. Pinned by
  `insert_edges_batch_reports_dropped_count_for_orphan_edges`:
  - Source present, target missing: NOT dropped; held for
    the federation's `project_repo` drain.
  - Source missing, target present: dropped (orphan — we
    can't project a source we don't have).
  - Source missing, target missing: dropped (orphan).
  - Source present, target present: inserted normally.

- **`assess_change` heuristic-caller count pinned.** The
  `~ N heuristic caller(s) included` line is the agent's
  only signal that a static-graph-empty blast radius has
  more callers than the static graph shows. Agents rely on N
  to decide whether to follow up with `explain_dispatch`; if
  N is wrong, the agent under- or over-estimates the blast
  radius. Pinned by
  `assess_change_heuristic_caller_count_reflects_graph`:
  fixture with three distinct File callers on a single
  Function target asserts `~ 3 heuristic caller(s) included`
  appears verbatim in the output.

- **Combined negative-coverage sweep across all 16 detector
  patterns.** A single fixture exercises every detector's
  benign form — `publisher_count`, `dispatch_count`,
  `container_size`, `Kafka` type usage, `app_size`,
  `router_count`, bare `spawn`, `evaluation_metric`,
  user-defined `Anything` trait — and asserts none of them
  fire. Each detector already has its own positive +
  negative tests; this combined sweep catches a future
  regression that widens any single pattern (e.g. removes
  a `\b` anchor) without requiring one new test per
  detector.

- **Cross-tool contract pinned: assess_change `low*` implies
  explain_dispatch sees the heuristic.** When assess_change
  emits `risk=low* — heuristic-only`, the same symbol fed
  to explain_dispatch must surface the heuristic caller
  (verdict `heuristic_only` or `runtime_confirmed`). If the
  two tools ever decouple, the agent would silently get a
  `no_callers` verdict on a heuristic-only fixture and ship
  the regression Tier 3 was built to prevent. Pinned by
  `assess_change_low_star_implies_explain_dispatch_sees_heuristic`.

- **Minimal OTLP HTTP/JSON ingest adapter for `runtime_trace`.**
  Closes the deferred Tier-3 OTLP listener work without
  pulling in `tonic` + `opentelemetry-proto`. Covers the
  common production path: OTel collector → OTLP HTTP
  exporter → lain. `parse_otlp_json(payload: &[u8]) ->
  Vec<SpanRecord>` walks the official OTLP HTTP/JSON shape
  (`{"resourceSpans": [...]}` → `scopeSpans: [...]` →
  `spans[]`) and emits one `SpanRecord` per span. Trace/span
  IDs must be 32/16 hex chars respectively; malformed IDs
  return `OtlpParseError::{TraceId, SpanId}`. Unknown span
  kinds fall back to `Internal` rather than dropping the span.
  What's intentionally NOT included: gRPC adapter (heavy
  deps) and the HTTP server route (a follow-up can wire
  `parse_otlp_json` to a hyper handler when
  `LAIN_TRACE_RUNTIME=true` is set).

- **OTLP HTTP listener at `/v1/traces`.** Wires the
  `parse_otlp_json` parser into a real `hyper::server::conn::http1`
  listener that OTLP collectors can POST to. The listener
  binds its own `TcpListener` (separate from the MCP HTTP
  transport) so the runtime trace path doesn't share the MCP
  bearer-token auth layer. POST `/v1/traces` ingests each
  span into the store; returns 200 with
  `{"partialSuccess":{"acceptedSpans":N}}` on success, 400
  on a malformed payload, 404 for any other path/method.
  Activation is opt-in via `LAIN_TRACE_RUNTIME=true`; the
  `cli/server.rs` wiring is left as a follow-up so this PR
  ships the listener + parser contract without changing
  startup behaviour. Auth on the listener is intentionally
  absent — OTLP collectors don't ship bearer tokens;
  operators firewall the port instead.

## [0.7.4] — 2026-09-16

### Added

- **Cosign keyless signing for release artifacts.** Each platform
  tarball now ships a `<tarball>.cosign.bundle.json` alongside the
  existing SLSA build-provenance bundle, signed via `cosign sign-blob`
  using the same GitHub Actions OIDC identity. Verifiable with only
  the `cosign` CLI — no `gh`/GitHub API dependency. See
  [`docs/VERIFICATION.md`](docs/VERIFICATION.md) for the verify
  command.

### Removed

- **Deleted the `v0.7.0`–`v0.7.3` GitHub releases.** They predated the
  build-provenance/SBOM/checksum pipeline and shipped without those
  artifacts. The underlying git tags are untouched; only the GitHub
  Release objects (and their binary assets) were removed.

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
