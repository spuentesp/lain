# Agent Surface Fixes — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the coordination layer actually coordinate, and stop the derived analyses from answering confidently when they have no data. Findings come from a live agent evaluation on 2026-08-21 (three Claude agents, three server instances, every answer re-derived from source with `grep`). Finding numbers below (`F-01` … `F-19`) refer to that report.

**Architecture:** Tasks 1–6 are the load-bearing ones and are independent of each other — each is a small change at a single boundary. Tasks 7–11 are truth-in-reporting fixes. Task 12 is a design decision that needs a call before it needs code.

**Branch:** `fix/agent-surface` off `main`.

---

## Global Constraints

- **No new Cargo deps.**
- **Every task lands with a regression test.** The bugs below all have the same shape: a behavior nobody pinned. `claim_files_schema_has_auth_args_and_typed_files` (`handler.rs:2584`) is the model — a test that fails loudly if the surface regresses.
- **Don't re-do finished work.** `1f3211f` already canonicalized *graph node* paths; `9756156` already deduped `find_anchors` by name. Tasks 1 and 6 are the *other* sites with the same bug class, not a redo.
- **Prefer degrading a claim over withholding an answer.** Where a tool can't know, it should say so in the payload rather than returning a confident wrong shape.

---

## Task 1: Canonicalize claim paths at the registry boundary (F-01)

**Files:** `src/server/presence.rs`, `tests/presence.rs`

**Goal:** Two agents claiming the same file conflict, regardless of how they spell the path.

**Context:** Claims are keyed on the raw `PathBuf` string. Four spellings of one file produced four independent grants and zero conflicts:

```
agent A → "/home/sebastian/lain/src/server/query/schema.rs"  → granted
agent B → "src/server/query/schema.rs"                       → granted, conflicts: []
agent B → "./src/server/query/schema.rs"                     → granted, conflicts: []
agent B → "src/server/../server/query/schema.rs"             → granted, conflicts: []
```

This is not theoretical drift. `lain hooks claim` writes **absolute** paths; MCP callers naturally write **repo-relative** ones. The CLI half and the MCP half of the product therefore occupy disjoint keyspaces and can never conflict with each other. The same split hits the filesystem fallback: `presence_lock::lock_path_for` sanitizes with `replace('/', "__")`, so the two spellings also produce two different sentinel files under `.lain/locks/`.

`OccupancyMap` already carries what the fix needs — `workspace_root: Arc<Mutex<Option<PathBuf>>>` (`presence.rs:597`), installed for the `presence_lock` side-effect.

- [x] **Step 1: Add a normalizer.** In `presence.rs`, next to `OccupancyMap`:

```rust
/// Canonical key for a claim path: workspace-relative, lexically
/// normalized, no `.` / `..` components. Paths outside the workspace
/// (or claims taken with no workspace root installed) keep their
/// lexically-normalized absolute form so they still collide with
/// themselves.
fn canonical_claim_path(root: Option<&Path>, path: &Path) -> PathBuf
```

Lexical normalization only — no `fs::canonicalize`. The claimed file may not exist yet (an agent claiming a file it is about to create), and a syscall per claim under the lock is a cost this path shouldn't pay.

- [x] **Step 2: Apply it once, at the entry point.** In `claim_with_session` (`presence.rs:668`) map every `ClaimRequest.path` through the normalizer before any keying, and use the same normalizer in `release`, `list_occupancy`, and the `presence_lock` side-effect so all four agree.

- [x] **Step 3: Echo back what was actually keyed.** `granted[].path` and `conflicts[].path` should return the canonical form, not the caller's spelling — otherwise an agent's `my_claims` won't match what it sent to `release_files`.

- [x] **Step 4: Regression test** — `claims_collide_across_path_spellings`:

```rust
for spelling in ["/abs/ws/src/a.rs", "src/a.rs", "./src/a.rs", "src/../src/a.rs"] {
    // second agent claiming any spelling conflicts with the first
}
```
Plus `claim_outside_workspace_still_collides_with_itself`, and one asserting `granted[].path` is canonical.

**Verify:** two agents, `--transport http`, one claiming absolute and one relative → `conflicts` is non-empty.

---

## Task 2: Put build identity in the health surface (F-03)

**Files:** `src/server/tools.rs`, `src/server/mcp/federation_tools/server_status.rs`, `build.rs`

**Goal:** An agent can tell that its server predates the fix it's reading about in the source tree.

**Context:** `claim_files` was uncallable on the stdio server this session was bound to — advertised `files: string`, handler wants a sequence. That bug is already fixed in-tree (`definitions.rs:173`, `envelope.rs`, test at `handler.rs:2584`). The server was simply older than the fix:

```
/proc/4121078/exe → /home/sebastian/lain/target/release/lain (deleted)
process started    Aug 19 00:58
binary on disk     Aug 21 11:58   ← rebuilt twice since; the process never restarted
```

MCP stdio servers are spawned once by the host and outlive every rebuild. `get_server_status` returns `pid` and `started_at` but no version, so nothing in the protocol surface can expose the gap. `lain doctor` checks exactly this — but it's a human CLI the agent never sees.

- [x] **Step 1:** Confirm `build.rs` stamps `LAIN_GIT_SHA` (it's already referenced in `src/`); add it if it only covers some build paths.

- [x] **Step 2:** Add `version`, `git_sha`, `binary_mtime_unix` to the `get_server_status` JSON and to the `get_health` markdown, next to `Last Enriched Commit`.

- [x] **Step 3: Detect the stale image.** At startup, stat `current_exe()` and keep the mtime. Serve a `binary_is_stale: true` flag (plus a line in `get_health`) when the file at that path is newer than the recorded mtime — i.e. the operator rebuilt under a running process.

- [x] **Step 4: Test** — `server_status_reports_build_identity` asserting the three keys are present and non-empty.

---

## Task 3: Any authenticated call refreshes the session (F-05)

**Files:** `src/server/presence.rs`, `src/server/mcp/presence_tools.rs`, `tests/presence.rs`

**Goal:** An agent doing ordinary work doesn't silently lose its claims.

**Context:** The TTL is 60 seconds of wall clock (`presence.rs:327`, `Self::with_expiry(Duration::from_secs(60))`) and only `heartbeat` resets it. Two controlled probes against the same server:

| probe | behavior | t+20 | t+40 | t+60 | t+66 | t+100 |
|---|---|---|---|---|---|---|
| `ttl-probe` | claims + polls, no heartbeat | alive | alive | — | **unknown session token** | dead |
| `hb-probe` | heartbeat every 20s | alive | alive | alive | alive | **alive** |

Successful `claim_files` and `my_claims` calls at t+20 and t+40 did **not** extend `ttl-probe` — expiry stayed pinned to registration. An LLM agent has no timer between turns, and a single turn with thinking plus a couple of round-trips routinely exceeds 60s. When the session dies the claims are dropped **silently**, so the next agent to ask gets `conflicts: []` on a file someone is mid-edit in.

- [x] **Step 1:** In the presence tool dispatcher, call `registry.heartbeat(agent_id, token)` on every tool call that authenticates — `claim_files`, `release_files`, `my_claims`, `who_am_i`, `list_subagents`. One line at the auth check, not per-handler.

- [x] **Step 2: Raise the interactive default** to 600s. Keep the short TTL available for `kind: cron | ci` agents via `with_expiry`, where a fast reap is correct.

- [x] **Step 3: Announce the reap.** When the expiry loop drops claims for a dead session, emit `PresenceEvent::ClaimRevoked { agent_id, path, reason: SessionExpired }` on the SSE bus so surviving agents and the Command Center see it. Today the claims just vanish.

- [x] **Step 4: Tests** — `tool_call_extends_session` (claim at t+0, claim at t+45 with a 60s TTL, still valid at t+90) and `expired_session_emits_claim_revoked`.

---

## Task 4: Stop attribution from manufacturing claims (F-04)

**Files:** `src/server/attribution.rs`, `tests/presence.rs`

**Goal:** Occupancy reflects what agents are doing, not what the filesystem is doing.

**Context:** A synthetic agent driven purely by `curl` — it registered, claimed one fake path, and heartbeat, nothing else — was credited with editing the git index:

```
hb-probe → my_claims
  [{"path":"src/hb_probe.rs","symbols":["hb_fn"]},
   {"path":"/home/sebastian/lain/.git/index.lock","claimed_at":1787341581}]
```

`.git/index.lock` was written by an unrelated shell command of mine. The phantom claim persisted for the agent's whole 100-second life. Cause is the documented fallback in `attribute_edit` (`attribution.rs:357`): when PID lookup fails and exactly one *interactive* agent is connected, **every** Modify/Create event under the watched roots is auto-claimed for that agent. Three problems compound:

1. No path filtering — the watcher registers roots with `RecursiveMode::Recursive` and the event loop only checks `path.is_file()`. A single `cargo build` would auto-claim thousands of `target/` artifacts.
2. Edits by non-agent processes (the human's editor, a shell script, git itself) land on whichever agent happens to be connected.
3. Auto-claims are built with `ttl_seconds: None`, so they never expire on their own — only session death clears them.

- [x] **Step 1: Filter before attributing.** In the event loop (`attribution.rs:~330`), skip paths containing `.git/`, `target/`, `node_modules/`, `.lain/`, `dist/`, `build/`, plus editor temporaries (`*.swp`, `*~`, `.#*`, `*.tmp`). Reuse the watcher's existing gitignore check (`watcher.rs:is_git_ignored`) rather than writing a second ignore implementation.

- [x] **Step 2: Give inferred claims a TTL.** Pass `ttl_seconds: Some(120)` from `attribute_edit` so a wrong guess self-heals.

- [x] **Step 3: Mark them inferred.** Add `inferred: bool` to `Claim` (default `false`), set it on the attribution path, and surface it in `list_occupancy` / `my_claims` / `conflicts`. A consumer should be able to weigh "this agent told me" differently from "the server guessed".

- [x] **Step 4: Considered narrowing the single-agent heuristic — and deliberately did not.** Narrowing it to files the agent already holds broke `attribution_auto_claims_via_pid_on_linux`, and the reason is instructive: PID lookup loses most real edits, because a write closes its fd long before the inotify event is handled. That test was passing *through* the heuristic, not through PID attribution. Narrowing would have disabled discovery — the feature's whole point — to fix a problem Steps 1–3 already fix. The harm was never the heuristic; it was what the heuristic was allowed to claim. `is_attributable` removes that class, and the remaining guesses are labelled `inferred` and expire on their own.

- [x] **Step 5: Tests** — `attribution_skips_git_and_build_dirs`, `inferred_claims_carry_ttl_and_flag`.

---

## Task 5: find_dead_code must not report unindexed files as dead (F-07)

**Files:** `src/server/tools/handlers/metrics.rs`, `tests/` (new)

**Goal:** The one tool whose wrong answer gets working code deleted stops guessing.

**Context:** `find_dead_code` returned **306 "highly confident dead symbols"**. Every one of the top 20 came from `src/server/watcher.rs`, tagged `[no callers, no callees]`. Five checked against source:

```
spawn_config_watcher       3 call sites   watcher.rs:985, :1019
run_watcher_thread         3 call sites   watcher.rs:172, :898
filter_event               4 call sites   watcher.rs:426
is_watched_file            1 call site    watcher.rs:562
discover_watch_directories 2 call sites   watcher.rs:446, :771
                           5 of 5 flagged dead are live
```

The bug is an inversion in the confidence model. `is_likely_false_positive` (`metrics.rs:112`) treats `fan_out > 0` as evidence of life, and `find_dead_code` then promotes `fan_in == 0 && fan_out == 0` to "highly confident". But a node with **both** counts zero is the exact signature of a file whose call extraction failed — no edges were recorded at all. So the highest-confidence bucket is precisely the unindexed bucket. Every missed call in `watcher.rs` is intra-file, several behind `super::` qualification or inside `#[cfg(test)]`; a second agent found the same failure through macro arguments (`sanitize_agent_name` is called inside a `format!(…)` and is likewise reported as a leaf).

- [x] **Step 1: Compute per-file edge density.** For each file with ≥1 function node, count outgoing `Calls` edges across all its nodes. Zero edges over a file with several functions means *unindexed*, not *dead*.

- [x] **Step 2: Exclude unindexed files from the result set**, and report them separately: `"12 files could not be call-indexed and were excluded: src/server/watcher.rs, …"`. That line is also the best bug report the extractor will ever get.

- [x] **Step 3: Rename the confidence tiers** so `fan_in == 0 && fan_out == 0` is no longer the top tier. A leaf with callers we can see is far better evidence than a node with no edges at all.

- [x] **Step 4: Make `like` honest.** `like:"presence claim"` returned the identical 306 count and identical rows as no filter — only the header word changed. When the embedder is in stub mode the filter silently degrades to a no-op; it must return the same `Unavailable` error `semantic_search` gives, naming the real remedy (see Task 9).

- [x] **Step 5: Test** — build a graph with one file whose nodes have no edges and one genuinely dead function; assert the unindexed file is excluded and reported, and the dead function is found.

**Verify:** `find_dead_code` on this repo no longer lists `watcher.rs`, and the count drops well below 306.

---

## Task 6: Aggregate co-change partners by path (F-08)

**Files:** `src/server/graph.rs`, `src/server/tools/handlers/architecture.rs`

**Goal:** No result list shows the same thing three times.

**Context:** `9756156` fixed this for `find_anchors` by keying on name alone. The same bug is live at the co-change site:

```
### Frequently Co-Changed With (Git History)
- src/server/presence_lock.rs (2 times)
- src/server/presence_lock.rs (2 times)     ← same file, three rows
- src/main.rs (2 times)
- src/server/presence_lock.rs (2 times)
```

`get_co_change_partners` (`graph.rs:1037`) maps raw outgoing `CoChangedWith` edges to `(target.path, weight)` with no aggregation, so duplicate `File` nodes for one path — or several edges to nodes sharing a path — each emit a row.

- [x] **Step 1:** Fold the result by `target_node.path` before returning, taking the max weight (not the sum — the weights are counts of the same underlying co-change relationship, and summing would inflate them).

- [x] **Step 2: Sweep the other aggregation sites** flagged in the evaluation: `explain_symbol`, `get_context_for_prompt`, `get_blast_radius`, `explore_architecture`. Each returns node lists straight from graph traversal without folding by path.

- [x] **Step 3: Fix the blast-radius count contradiction.** Already landed upstream in `f3eb0b8` ("blast radius dedup/count/port"), after the index the evaluation ran against: `impact.rs:151` now derives `total_affected` from `affected_names.len()`, the same list the truncation notice counts. The `… and 109 more` / `Total: 24` split was the stale binary, not live code. Verified, no change needed.

- [x] **Step 4: Test** — `co_change_partners_are_deduped_by_path` with a graph containing two `File` nodes for one path.

---

## Task 7: Make the declared schema match the graph (F-09)

**Files:** `src/server/query/schema.rs`, `src/server/tools/handlers/metrics.rs`, `src/server/tools/handlers/navigation.rs`

**Goal:** An agent that follows `describe_schema` gets results.

**Context:** `describe_schema` lists five node types — Function, File, Module, Class, Interface. The graph also holds `Method` (`lsp.rs:376` maps `SymbolKind::METHOD` to it). Following the documented schema produces a flat contradiction:

```
find_anchors                                        → as_str is the #1 anchor
query_graph {find, type:"Function", name:"as_str"}  → count: 0
query_graph {find,                  name:"as_str"}  → count: 6, type:"Method"
```

In a Rust codebase most logic lives in `impl` blocks, so this isn't an edge case. `Method` is also missing from the analysis node-type arrays at `metrics.rs:382` (`suggest_refactor_targets`) and `navigation.rs:229` — part of why refactor targets miss the real monsters.

- [x] **Step 1:** Add `Method` to `node_types` in `schema.rs` with an accurate description.

- [x] **Step 2:** Add `NodeType::Method` to the arrays at `metrics.rs:382` and `navigation.rs:229`.

- [x] **Step 3: Prevent the drift from recurring.** Derive the `describe_schema` list from the `NodeType` enum (a `NodeType::all()` + `describe()` pair) so a new variant can't be added without appearing in the schema.

- [x] **Step 4: Test** — `describe_schema_covers_every_node_type` iterating `NodeType::all()`.

- [x] **Step 5 (found while fixing Step 1): the edge list had drifted further than the node list.** `describe_schema` advertised `Defines`, `Inherits`, `TestedBy` and `Import` — none of which are `EdgeType` variants — and omitted eight that are, including `CoChangedWith`, the temporal coupling lain advertises as a headline feature. Same treatment: `EdgeType::all()` / `description()` / `source_types()` / `target_types()`, with `describe_schema_covers_every_edge_type` asserting the list matches the enum in both directions.

**Note:** with `Method` included, re-check `suggest_refactor_targets` on this repo. It currently flags a 69-line shell script and `default` in `tuning.rs` (34 unrelated `fn default` impls collapsed to one node, manufacturing fan-in) while missing `handler.rs` (3,050 lines), `presence.rs` (1,707), `graph.rs` (1,608). Task 6's dedup plus this task should move those.

---

## Task 8: Toolchain spawn errors that name the program (F-15)

**Files:** `src/server/tools/handlers/execution.rs`

**Goal:** A failed `run_build` tells the agent what to fix.

**Context:** All three execution tools fail with an error that names nothing:

```
run_build{cwd:"/home/sebastian/lain"}  → Error: IO error: No such file or directory (os error 2)
run_tests{cwd:"/home/sebastian/lain"}  → Error: IO error: No such file or directory (os error 2)
```

`cargo` lives at `~/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo` and is **not on the server process's PATH** — the spawn fails, not the cwd. `cmd.output().await.map_err(|e| LainError::Io(e.to_string()))` (`execution.rs:201` and siblings) stringifies `ErrorKind::NotFound` into exactly that message. MCP servers inherit the host's PATH, which for editor-launched processes routinely lacks toolchain shims. For contrast, a bad cwd gives a genuinely good error: `"No profile found for toolchain: unknown. Add a toolchains/unknown.toml file."`

- [x] **Step 1: Wrap the spawn error.** On `ErrorKind::NotFound`, return `NotFound` naming the program, the resolved cwd, and the PATH the server is running with.

- [x] **Step 2: Resolve the toolchain binary** before falling back to bare PATH: `$CARGO`, then `rustup which cargo`, then `~/.cargo/bin/cargo`. Apply at all three call sites (`build`, `test`, `clippy`) — the resolution belongs in `toolchains.rs` next to the profile lookup, not duplicated per handler.

- [x] **Step 3: Test** — spawn with a deliberately empty PATH and assert the error contains the program name.

---

## Task 9: Delete the phantom command (F-16)

**Files:** `src/server/tools/handlers/registry_impl.rs`

**Context:** `registry_impl.rs:326` tells agents to run `lain install-embeddings`, which does not exist — the `Commands` enum has `Server, Workspaces, Repos, Query, Oneshot, Ask, Mcp, Init, Hooks, Doctor`. This is already logged as a to-do in two plan docs (`2026-08-17-release-hygiene.md:146`, `2026-08-17-federation-tool-wiring.md:501`) with the right diagnosis: *"an agent that follows the instruction gets an error, and then has to decide whether to trust the next thing lain tells it."* Still shipping.

- [x] **Step 1:** Replace with the paths that work — `install.sh --download-model`, the `LAIN_EMBEDDING_MODEL` env var, or dropping a model into `.lain/models/`.
- [x] **Step 2:** Grep for other invented commands in error strings; add a test asserting every `lain <subcommand>` mentioned in a user-facing string parses against the clap `Commands` enum.

---

## Task 10: Advisory on read-over-edit (F-06)

**Files:** `src/server/presence.rs`, `src/server/mcp/presence_tools.rs`

**Context:** Agent A held `handler.rs` with intent `edit`; agent B asked for it with intent `read` and got `{"conflicts":[],"granted":[…]}` — no mention of A. Not blocking readers is right. Telling them nothing is not: this is the most common real failure for agent teams — B reads, reasons for two minutes, and patches a version A already replaced.

- [x] **Step 1:** Add `advisories: Vec<Conflict>` to `ClaimResult` — same shape as `conflicts`, explicitly non-blocking — populated when a granted read overlaps a live edit claim.
- [x] **Step 2:** Document in the `claim_files` tool description that `advisories` is informational, so an agent knows it may proceed but should re-read before patching.
- [x] **Step 3: Test** — `read_over_edit_grants_with_advisory`.

---

## Task 11: Truth-in-reporting sweep (F-10, F-11, F-14, F-19)

**Files:** `src/server/tools.rs`, `src/server/mcp/federation_tools/server_status.rs`, `src/server/mcp/definitions.rs`, `src/server/ingest/constructors.rs`

Small, independent, each a lie an agent currently has to work around.

- [x] **Step 1: Don't report ✅ while degraded (F-10).** `tools.rs:417` hardcodes `Status: Operational ✅` in the format string. This session's server printed it for two days while serving a graph 94 files behind HEAD, with `⚠ startup re-index failed` in the same payload. Derive the status from re-index state: `Operational ✅` / `Degraded ⚠ (serving a stale graph)`.

- [x] **Step 2: Don't put durable state in `/tmp` (F-10).** `allocate_staging_dir` (`constructors.rs:41`) builds the federation placeholder at `/tmp/lain-federation-{pid}-{counter}` and `git2::Repository::init` leaves HEAD on an unborn `refs/heads/master` — which is the exact text of the startup error. `/tmp` cleanup then removed the directory out from under the running server. Use the state dir already computed for `.lain/`; keep the pid-counter suffix for test isolation.

- [x] **Step 3: Make `sync_state` observable (F-10).** It returns `"State sync started in background. Check 'get_health' later."` and then nothing changes — `get_reload_status` stayed `{"state":"idle","last_error":null}` and the enriched commit never moved. Return a `job_id` resolvable through `get_job_status`, and record failures where `get_health` will show them.

- [x] **Step 4: Add staleness to not-found answers (F-10).** `run_oneshot`, `check_bearer` and `resolve_repos_config` all returned a flat `symbol not found in any repo` — an agent reads that as "this function does not exist". Append `"(graph is N commits behind HEAD; the symbol may be newer than the index)"` whenever the index trails. `get_blast_radius` already prints an `Overlay freshness: stale` note; `get_call_sites`, `explain_symbol`, `find_dead_code`, `explore_architecture` and every not-found path do not.

- [x] **Step 5: Distinguish absent from retracted (F-14).** `get_world_state{symbols:["claim_files"]}` returned `change_kind: "Retracted"` for a symbol that was never a graph node (it's a match arm). The tool exists to answer "is this symbol still in the graph?" before claiming; conflating *never indexed* with *deleted* tells an agent its target was removed out from under it. Add `NotIndexed` as a distinct `change_kind`.

- [x] **Step 6: Stop reporting a port nobody listens on (F-19).** `get_server_status` returns `{"transport":"stdio","port":9999}`. Return `port: null` under stdio.

- [x] **Step 7: Declare every documented argument (F-19).** Several descriptions promise args the JSON schema omits, so a schema-respecting client can't send them: `list_active_agents` documents `include_background`, `detect_overlap` documents `head`. Add them to `required_args` / the property schema, or drop them from the prose.

- [x] **Step 8: Give `detect_overlap` a remedy (F-19).** It's advertised unconditionally in `tools/list` and returns `"no workspaces file loaded on this server"`. Either hide it when no workspaces file is loaded, or name the fix (`lain workspaces create …`).

- [x] **Step 9: Fix `explore_architecture`'s labels (F-12).** `max_depth: 2` and `max_depth: 3` return byte-identical output, and the per-group counts describe the truncated top-20 rather than the directory — so an onboarding agent reads `### src/ (1 files)` for a directory holding 144 `.rs` files. Either honor `max_depth` or drop the parameter; either way label counts as `showing N of M`.

- [x] **Step 10: Reconcile contradicting tools (F-11).** In one session, `get_call_sites("sanitize_agent_name")` said *"is a leaf"* while `explain_symbol` said *"Called by: hooks.rs"*. And `trace_dependency("presence.rs")` returned `{"error":"ambiguous_symbol","candidates":["lain","lain"]}` — one repo is registered, the duplicate is the same repo twice, and the tool schema has no `repo_id` parameter to pass. An error must never name a remedy the API doesn't offer: either add the parameter or change the message.

---

## Task 12: One registry for stdio clients (F-02) — decide before coding

**Files:** `src/server/presence.rs`, `src/cli/mcp.rs`, `src/cli/server.rs`, `README.md`

**Goal:** Two agents on one repo can see each other in the configuration the README recommends.

**Context:** Presence is per-process and nothing is persisted. The MCP stdio transport spawns one server **per client**, so two Claude Code windows on the same repo run two servers, two registries, and zero shared knowledge — while every tool keeps answering successfully. Registered on stdio, invisible on HTTP twenty seconds later:

```
stdio  → register_agent("stdio-probe-XYZ") → live, expires_at 1787341572
:9992  → list_active_agents → [sub-alpha, rival-beta, ttl-probe]   ← XYZ absent
:9990  → list_active_agents → []                                   ← a third registry
find .lain/ ~/.local/lain/ -newermt '-10 minutes' → (no presence file)
```

The README's "Wire your agent" section configures stdio. So the default install produces exactly the topology where multiplayer cannot work, and nothing signals it.

- [x] **Step 1: Pick one.**
  - **(a) Persist presence to a workspace file.** `<state_dir>/presence.json` with advisory locking, re-read on each presence call. Cheap, works for any number of processes, but adds an fsync to a hot path and needs careful reap semantics for dead pids.
  - **(b) stdio discovers and proxies.** `lain mcp` / `lain server --transport stdio` looks for a running HTTP server for this workspace and forwards presence calls to it, spawning one if absent. Keeps a single in-memory registry — which is where the good conflict semantics already live — at the cost of a supervision story.
  - **(c) Document the limitation.** State plainly that coordination requires `--transport http` against one shared server, and have the stdio path warn on first `claim_files` that it is a private registry.

  **Chosen: (a), on the user's instruction to optimize for a single-computer MCP.** It turned out to be the smallest correct change as well: `save_pair` / `load_pair` already round-trip the whole registry through `<state_dir>/<workspace>.json`, and `install_persist_callback` already wrote on every mutation. The file was only ever *written*, never re-read. What was missing was refresh-before-act plus a lock so a read-modify-write cycle can't clobber a peer — added as `server::state_lock` (an `O_EXCL` sentinel, no new dependency) and `LainServer::with_shared_presence` / `refresh_shared_presence`.

  (b) was rejected for this milestone: proxying needs a supervision story (who spawns the HTTP server, who reaps it, what happens on crash) that buys nothing extra on one machine. (c) is subsumed — the limitation is gone rather than documented.

  Advisory throughout: a lock timeout proceeds unlocked and a failed load/save is logged, because a presence registry that can wedge a tool call is worse than one that occasionally loses a write.

- [x] **Step 2:** Implement the choice, with a two-process integration test: two servers over one workspace, agent A claims in one, agent B conflicts in the other.

- [x] **Step 3: Reconcile the CLI identity (F-18).** `lain hooks claim` registers a *distinct* `agent_id`, so claims taken through the CLI never appear in the same agent's `my_claims` over MCP. Also: `--url` is mandatory while `LAIN_URL` is read elsewhere in the codebase and ignored here, and `claim` takes one `--path` per invocation so a multi-file claim can't be atomic. Accept `LAIN_URL`, accept repeated `--path`, and let the hook reuse a session token from `<hooks_dir>/<agent>.session` (the file `write_session` already maintains).

---

## Suggested order

Tasks 1–6 are independent; ship in whatever order suits. If serializing:

| # | Task | Why here |
|---|---|---|
| 1 | Task 1 — canonical claim paths | One function at one boundary. Without it the whole coordination layer is decorative. |
| 2 | Task 2 — build identity | Cheap, and it makes every future bug report interpretable. Half of what looked broken in the evaluation was a two-day-old process. |
| 3 | Task 3 — session refresh | Removes a class of silent claim loss no LLM agent can engineer around. |
| 4 | Task 4 — attribution ignore-set | Stops phantom claims polluting occupancy, and stops a build from flooding the registry. |
| 5 | Task 5 — dead code guard | The one tool whose wrong answer destroys code. |
| 6 | Task 12 step 1(c) — document the stdio limitation | One paragraph; converts a silent failure into a known one while (a)/(b) is decided. |

Tasks 6–11 are low-risk and can be batched into a single cleanup PR.

---

## Task 13: Index convergence — fixed

**Status:** done. Two distinct defects, each reproduced with a failing test first, then fixed.

**What I set out to fix:** the call-extraction gaps assumed behind F-07 — intra-file calls, `super::`-qualified calls, calls inside `format!`. **That assumption was wrong**, and the correction matters more than the original guess.

- [x] **The extractor is not the problem.** Running `treesitter::extract_refs` directly over `src/server/watcher.rs` — the file that supplied all 20 "highly confident dead symbols" — yields **189 call refs**, including every symbol reported dead: `spawn_config_watcher` ×2, `run_watcher_thread` ×3, `filter_event` ×3, `is_watched_file` ×1, `discover_watch_directories` ×2. `audit.rs` yields 54, `cli/query.rs` 123.

- [x] **The resolver is not the problem either.** Feeding those refs through `resolve_static_edges` against nodes built from `extract_definitions` (so they carry line ranges) produces edges: audit.rs 54 refs → 6 call edges, query.rs 123 → 3.

- [x] **`get_node_at_location` works** on the production graph: `at_location("src/cli/query.rs", 20)` correctly returns `run_query`, whose range is 13..54.

- [x] **The loss is in the index.** In one production graph (3769 nodes), **37 of 335 files had symbols but zero `Contains` edges from their own file node** — their symbols are orphaned, and most have no edges of any kind. `run_query`, `walk_up_for_git` and `append_edit_event` each had *zero* incoming and outgoing edges despite being demonstrably called.

- [x] **Indexing is non-deterministic.** Two indexes of the same commit (`9756156`) with the same binary produced **3769 nodes / 9869 edges** and **3340 nodes / 15540 edges**. Same repo, same code, materially different graphs. Every metric derived from the graph inherits that variance.

- [x] **Fixed the observability gap that let this hide.** `insert_edges_batch` skipped edges whose endpoints were missing from the index and returned `Ok(())` — no error, no count, no log. It now returns the number dropped, and every ingestion call site logs a non-zero count. An indexing pass that drops edges produces a graph that does not describe the code, and that must not be silent.

- [x] **Defect 1: incremental re-index destroyed inbound edges (candidate 1, confirmed).** `replace_nodes_for_paths` removes a path's nodes and petgraph takes every incident edge with them — including *incoming* edges from files the pass is not rebuilding. An incremental pass only re-resolves refs from the files it scanned, so a caller in an untouched file was never restored. Reproduced by `incremental_reindex_keeps_callers_in_unchanged_files`: 1 inbound `Calls` before, **0** after.

  Fixed by capturing inbound edges whose source is *outside* the replaced set before removal, then restoring them after the swap inside the same write lock. Node ids are deterministic, so a symbol that survives the re-scan returns under the same id and the edge is still true; a symbol genuinely deleted does not return and the edge stays dropped. Boundary tests: `deleting_a_symbol_still_drops_its_inbound_edges`, `repeated_reindex_does_not_accumulate_duplicate_edges`.

- [x] **Defect 2: deletion-only commits stranded nodes permanently.** `get_changed_files_since` deliberately skips paths no longer on disk, so a commit that only deletes files yields an empty scan list. Both pipelines then took a `files.is_empty()` early return that advanced the last-commit marker **and skipped the orphan sweep** — so the deleted file's symbols stayed in the graph and no later pass revisited that commit. This is why a long-lived index carried *more* nodes than a fresh one of the same commit. Reproduced by `deleting_a_file_removes_its_nodes`; fixed by extracting `sweep_orphans` and calling it on both exits, in the federation and single-workspace pipelines alike.

- [x] **Candidate 2 (scan cap / `abort_all`) ruled out** for this repo: a clean full index reports **zero** dropped edges and zero collateral removals.

- [x] **Determinism confirmed** by `two_full_indexes_of_one_commit_agree`. The original 3769/9869-vs-3340/15540 discrepancy was the two defects above plus differing index histories, not scan non-determinism.

- [x] **Measured on this repo, fresh index, before → after:** `Calls` edges **4491 → 9870**; files reported as an indexing gap **22 → 1**; symbols wrongly reported dead **306 → 0**. The one still flagged is `glob_match.rs` (one real function plus four `#[test]`s); its tests come from the LSP path, which does not set the `test` label, so it trips the >=3-function threshold. That failure is conservative — the file is excluded from dead-code reporting rather than falsely accused — and closing it means propagating `#[test]` labels onto LSP-derived nodes.

- [x] **Bonus: the `refs/heads/master` warning is gone.** `allocate_staging_dir` left the federation placeholder repo on an unborn HEAD, so the startup re-index failed on every boot against a directory holding no code — and after Task 11 that drove a permanent, *false* `Degraded`. The placeholder now gets an empty initial commit, so the re-index is a trivial no-op and a degraded status once again means something is wrong. Verified live: `Status: Operational` with no warning line.

- [ ] **Open (unchanged): `fan_in` / `fan_out` semantics.** They count *all* edge kinds, not just `Calls`, while `find_dead_code` reads them as call counts. On the graph checked this happened to be harmless (every `fan_in == 0` function genuinely had zero incoming `Calls`), so it is a latent correctness trap rather than a live bug — but the two should not be conflated.

---

## Out of scope
- `Method` node semantics beyond registering the type (Task 7). Whether a method should be an anchor candidate is a scoring question, adjacent to the hub-scoring formula documented in `GraphDatabase::calculate_anchor_scores`.
- Semantic search quality. The model wasn't loaded in this evaluation, so `semantic_search` was never exercised — only its error message (Task 9).

---

## Review 2026-08-22 (post-implementation, independent)

Everything through Task 13 landed in `29d5ebc` (42 files, +4034/−317);
full test suite green (`cargo test` exit 0) on that tree. Verified each
task against the committed diff — all implemented, with these findings:

**Gaps to close:**
- Task 1: the plan's `claim_outside_workspace_still_collides_with_itself`
  test was never written (the other three path-spelling tests exist in
  `tests/presence.rs`).
- Task 6 step 2: `get_context_for_prompt` still returns node lists
  unfolded by path (the other aggregation sites were handled via Tasks
  11.9/11.10, not path folding).
- Task 11 step 4: staleness note on not-found answers only exists in the
  federation resolver; `get_call_sites`, `explain_symbol`,
  `explore_architecture` and the single-repo not-found paths still
  answer flat "not found".

**Deviations (acceptable, noted for the record):**
- Task 4: own ignore-set in `attribution.rs` instead of reusing
  `watcher::is_git_ignored`.
- Task 8: `resolve_program` lives in `execution.rs`, not
  `toolchains.rs`.
- Task 12: the two-process test is two `LainServer` instances in one
  test process sharing the state file — same file semantics, not OS
  processes.

**Landed but not in the plan (documented here so the plan stays the
record):** re-claim replaces rather than accumulates
(`presence.rs:1070`), `overlap_check` CLI accepts `LAIN_URL`,
`reload.rs` test teardown for the staging-dir change.

**In flight at review time (uncommitted, NOT part of this plan):**
`src/server/sentinel.rs` (new module, 137 lines) replacing much of
`state_lock.rs` (−84 lines there) plus `ingestion.rs` changes. Needs
its own plan entry or a follow-up commit before any merge to main.

**Still open:** Task 13's `fan_in`/`fan_out` semantics note (counts all
edge kinds; `find_dead_code` reads them as call counts). Latent, not
live.
