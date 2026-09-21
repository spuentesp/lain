# Follow-ups

Current work deliberately deferred from changes already merged to `dev`.
Completed plans, audits, and implementation notes live in Git history rather
than this file.

Last verified against `dev` on 2026-09-21.

## Distribution: Windows clean-room install

- **Status:** fix landed in tree; awaiting the next release tag.
- **Evidence:** `release.yml::build-windows` packages every
  `*.dll` from `target/x86_64-pc-windows-msvc/release/` alongside
  `lain.exe`, and the build fails loudly if `DirectML.dll` is
  missing. `npm-shim/scripts/install.test.js` has the
  `Windows install fails clearly when DirectML.dll is absent`
  regression fixture. The published artifact is still the
  pre-fix `lain.exe`-only archive until the next tag carries the
  corrected packaging.
- **Work:** ship the corrected archive in the next release and
  verify it from a clean Windows runner.
- **Acceptance:** all six scheduled user/automation lanes pass and
  the release gate remains green for all three release targets.

## Runtime traces: federation-aware OTLP resolver

- **Status:** completed (2026-09-21).
- **Evidence:** `runtime_trace::server::federation_resolver(fed:
  Arc<FederatedIndex>) -> SpanResolver` walks every registered
  repo's graph looking for the span name, honors OTLP semconv
  hints (`code.repo` for repo-narrowed resolution;
  `service.name` as a secondary lookup key), and returns `None`
  on ambiguity rather than minting a wrong-repo edge. Wired
  through `cli/server.rs`. Regression:
  `federation_resolver_narrows_via_code_repo_attribute`.

## Indexing: LSP subprocess isolation

- **Status:** blocked on the upstream `lsp-bridge` API.
- **Background:** libgit2, tree-sitter, and ONNX work has moved off Tokio
  worker threads. LSP round trips remain async-only but are cancellation-aware.
- **Work:** file the prepared request in
  [`UPSTREAM_LSP_BRIDGE_ISSUE.md`](UPSTREAM_LSP_BRIDGE_ISSUE.md), then migrate
  the local call sites after upstream exposes blocking entry points or the raw
  stdio transport.
- **Acceptance:** hot LSP calls run outside Tokio worker threads while retaining
  cancellation and timeout behavior.

## Coordination: stdio lock fail-open concurrent-write race

- **Status:** fixed.
- **Background:** `harness/reproduce-stdio-lock-fail-open.py` forces the
  state-file lock sentinel and runs N agents through the lock acquisition's
  2-second timeout. All N agents then proceeded unlocked
  (`with_shared_presence`'s historical fail-open path), each read the
  same pre-claim snapshot, each ran `claim_files`, and each fired its
  persist callback. Last writer wins on disk; every agent returned
  "granted" to the caller even though the linearizability invariant from
  `docs/archive/COORDINATION_CONSISTENCY_PLAN.md` says at most one live
  exclusive lease per scope. The user's report also identified that
  `state_lock::acquire` used `PresenceConfig::default()` instead of the
  loaded `tuning.toml`, and that `run_lain_intent` and `handle_hook`
  wrote to the same state file *outside* `with_shared_presence` so their
  persist callbacks raced with claim writes under natural contention.
- **Fix:**
  - `state_lock::acquire_with` takes operator-supplied timeouts; the
    old `acquire` is preserved as the default-timeouts wrapper for
    back-compat.
  - `PresenceLayer` now carries a `lock_timeouts: LockTimeouts` snapshot
    of `PresenceConfig` taken at construction; `with_shared_presence`
    threads those into `acquire_with` so `.lain/tuning.toml`'s
    `state_lock_*` keys actually take effect.
  - `with_shared_presence` returns
    `Result<T, CoordinationError>`; on timeout it returns
    `Err(CoordinationError::Unavailable)` instead of proceeding
    unlocked. Exclusive mutations (`run_register_agent`,
    `run_heartbeat`, `run_claim_files`, `run_release_files`,
    `run_lain_intent`, `handle_hook`) propagate the error to the
    agent so it can retry; the agent contract surfaces
    "coordination_unavailable" as a tool error.
  - `run_lain_intent` and `handle_hook` now wrap their mutations in
    `with_shared_presence` so the intent-declare and
    activity-record persist callbacks fire under the same lock as
    claim writes.
  - `HookEvent`, `LainIntentArgs`, and the four presence-tool args
    structs gained `Clone` derives so the closures can be `FnOnce`.
- **Reproducer results:**
  - `harness/reproduce-stdio-lock-fail-open.py --natural
    --agents 6 --iterations 3`: every iteration reports 1 grant + 5
    conflicts, exactly the linearizability invariant.
  - `--natural --agents 2 --iterations 5`: every iteration reports
    1 grant + 1 conflict.
  - Forced-lock-timeout: every iteration reports 0 grants + 6 errors
    (all agents fail-closed).
- **Test counts:**
  - `cargo test --lib`: 1089 passed (1 new: `with_shared_presence_fails_closed_when_lock_unavailable`).
  - `tests/coordination_linearizability::linearizability_holds_across_n_agents_and_n_iterations`:
    400 races, 0 invariant violations.
  - `scripts/e2e_full.sh`: 46 / 46.
  - `scripts/agy_chaos.sh` variant 1: bob_granted_post_restart = 1.
- **Acceptance:** reproducer reports `issue not observed` under
  forced-timeout; natural contention reports exactly 1 grant + (N − 1)
  conflicts per iteration; `with_shared_presence` returns
  `Err(RefreshFailed | PersistFailed)` when the state file can't be
  read or written (e.g. the path was replaced by a directory between
  two agents registering).

## Planned capability expansions

These are scoped plans, not partially implemented promises:

- [`hybrid-lsp-expansion.md`](hybrid-lsp-expansion.md): LSP implementation,
  type-definition, and call-hierarchy edges.
- [`otlp-grpc-ingest.md`](otlp-grpc-ingest.md): optional OTLP gRPC/protobuf
  ingest alongside the existing lightweight HTTP/JSON path.

Each plan should be updated or removed when its implementation lands.

## Trust and release work

- **Dependency advisories:** keep the current actions in
  [`VULNS.md`](VULNS.md).
- **OpenSSF gaps:** keep the current measurements and process decisions in
  [`SCORECARD.md`](SCORECARD.md).
- **Agent-contract badge:** optional polish. The commit status is live and used
  by branch protection, but there is no distinct badge endpoint. Only build one
  if the README needs a separate signal from the ordinary CI badge.
- **Next release:** `dev` is ahead of `main`; choose the next version, create a
  `release/v0.x.y` branch from `dev`, update all release metadata, and open the
  release PR against `main` as described in [`BRANCHING.md`](BRANCHING.md).

## Maintenance rule

Add only concrete unfinished work with evidence and acceptance criteria. Remove
an entry when it lands; the PR and Git history are the archive.

## Coordination: intent + observability layer

- **Status:** **completed** (2026-09-21). All six PRs of the
  design plan landed; the regression fixtures pass. The plan was
  archived to [`docs/archive/INTENT_AND_OBSERVABILITY_PLAN.md`](archive/INTENT_AND_OBSERVABILITY_PLAN.md)
  once the work shipped. Open follow-ups are listed under
  "Next (open follow-ups)" below.
- **Evidence:**
  - Unit tests in `server::evaluation` (11 cases) cover the
    GREEN / YELLOW / RED rule table.
  - Integration tests in `tests/hook_ingest.rs` (5 cases) cover the
    `POST /hook` wire shape (auth, validation, activity recording).
  - Linearizability stress in `tests/coordination_linearizability.rs`
    (4 cases): 100 iterations × 4 racers, 50 release/reclaim
    cycles, N=10 stress, non-overlapping negative control.
  - End-to-end fixture `scripts/e2e_full.sh` drives a real
    `lain server` through 46 scenarios across all seven groups
    (intent lifecycle, activity observation, evaluation engine,
    persistence, cross-agent, error paths, doc accuracy).
  - Promise harness `scripts/test_all_promises.py` (22 promises)
    verifies every documented multiplayer/intent/hook claim
    against the live server.
  - Use cases harness `scripts/use_cases_e2e.py` (18 use cases)
    exercises every documented end-to-end flow.
  - AGY end-to-end `scripts/agy_e2e.sh` writes `verdict.json` for
    the real-agent harness regression fixture.
  - In-memory state survives `save_state` / `load_state`
    round-trips through the existing presence snapshot.
- **Linearizability invariant:** preserved. The `evaluation::evaluate`
  function refuses to downgrade a held claim (RED always wins over
  YELLOW peer-reading), and the existing file-lock primitive from
  [`docs/archive/COORDINATION_CONSISTENCY_PLAN.md`](archive/COORDINATION_CONSISTENCY_PLAN.md)
  continues to be the source of truth for exclusive ownership.
- **Next (open follow-ups):**
  - **Chaos variants in the AGY harness.** The harness
    (`scripts/agy_chaos.sh`) now runs three variants:
    kill-the-winner (variant 1 — surfaces a real
    linearizability gap, see below), corrupt-the-state-file
    (variant 2), and stale-lock-takeover (variant 3). Each
    variant writes a verdict JSON under `$OUT_DIR/variant_N.json`.
    The variant-1 finding is tracked below.
  - **Per-agent-kind hook wrappers for activity observation.**
    Landed. `hooks/agy/pre-tool.sh`, `hooks/codex/pre-tool.sh`,
    and `hooks/kimi/pre-tool.sh` are bash, fail-open, parse the
    agent's stdin JSON envelope, and forward every tool call
    (Read / Grep / Bash / Edit) to `lain hooks observe` which
    POSTs `/hook`. The CLI subcommand is the agent-agnostic
    replacement for hand-rolled curl in the per-agent wrapper.
  - **Graph-distance refinement in the evaluation engine.**
    Landed. `EvalContext::graph: Option<&GraphDatabase>` is
    threaded through the evaluator; `scope_distance` now
    consults the static graph via BFS over `Calls` edges
    (32-hop cap), with lexical `path_distance` as the fallback.
    Regression: `graph_distance_wins_over_lexical_when_graph_supplied`.
  - **Synchronous pre-edit hook endpoint.** Landed.
    `POST /hook/evaluate` wraps the existing evaluator; takes
    `body: Value`, validates `session_token` / `agent_id` /
    `target`, returns `Level + reason + related[]` synchronously.
    Module: `src/server/mcp/hook.rs::evaluate`.

## Coordination: linearizability across server crashes

- **Status:** completed (2026-09-21).
- **Evidence:** `presence::load_pair` in `src/server/presence.rs`
  now cross-checks every claim's `agent_id` against the freshly
  loaded `PresenceRegistry::sessions` and drops claims whose
  owner is no longer registered. The reclaimed paths are
  returned as `Vec<PresenceEvent>` of
  `ClaimRevoked { reason: "stale_owner" }` events;
  `PresenceLayer::load_state` forwards them to the SSE
  broadcast channel so peers see the same view the new server
  has. Regressions in `tests/presence.rs`:
  - `load_pair_reclaims_orphaned_claims_on_fresh_server` —
    alice's session is purged before save; the load drops
    her claim and emits exactly one `ClaimRevoked` event.
  - `load_pair_keeps_claims_for_live_agents` — alice's
    session survives the save; the load keeps her claim and
    emits zero events.
  - `a_conflict_from_a_departed_holder_reports_a_null_name`
    (updated to assert the new reclaim behavior) — phantom
    holders are reclaimed so the next claim succeeds.
  End-to-end: `scripts/agy_chaos.sh` variant 1 now produces
  `bob_granted_post_restart == 1` after the harness edits
  the state file to simulate heartbeat expiry. `variant_1.json`
  records `finding: "OK: load_pair reclaims alice orphan claim;
  bob wins."`
