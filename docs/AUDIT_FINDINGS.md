# Codebase audit findings — full review

This document collects every bug and improvement surfaced by the
whole-codebase review across the audit PRs (already shipped) and
this final sweep. Each item has a severity, file/line reference,
and a recommended placement in the PR queue.

## Status snapshot

Six audit PRs have shipped to upstream branches but are not yet on
`dev` (they wait in open PRs #206-#211). The remaining ~30
findings from this final sweep are listed below, prioritised.

| Severity | Meaning |
|----------|---------|
| P0       | Production reliability: server hang / crash / data loss on a common path |
| P1       | Silent correctness bug: produces wrong results with no signal |
| P2       | Defensive / error-handling: better diagnostics or boundary safety |
| P3       | Code quality: dead code, lint, naming, minor refactor |

## P0 — must fix

### F1. Tool execution timeouts orphan child processes
**Files:** `src/server/tools/handlers/execution.rs:142-152, 237-240, 318-328`
`run_build` / `run_tests` / `run_clippy` wrap `cmd.output()` in
`tokio::time::timeout(...)` but don't set `kill_on_drop(true)` or
explicitly call `cmd.kill()` on timeout. A stuck linker or hung
test leaves the underlying `cargo`/`pytest`/`clippy` process running
in the workspace indefinitely, holding the `target/` lock, file
handles, and a PID. This is the exact "MCP call never returns"
failure mode the timeout was supposed to bound.

**Fix:** set `cmd.kill_on_drop(true)` on the three `Command`s.

### F2. Federation `index_one_repo` is not fail-fast on wedged libgit2
**Files:** `src/server/ingest/ingestion.rs:1203, 1240, 1246, 1249, 1466`
The federation path calls the *blocking* git helpers
(`get_latest_commit_info`, `get_all_tracked_files`,
`get_changed_files_since`, `analyze_co_changes`) instead of the
`try_*` variants the single-workspace pipeline uses (e.g.,
`try_analyze_co_changes` at line 609). A wedged spawn_blocking
thread holding the parking_lot `GitSensor` mutex blocks
indefinitely; only the outer `index_timeout()` budget (60 s default)
eventually frees it. The doc comment on
`IngestHandle::start_git_sensor_watchdog`
(`src/server/ingest/handles/ingest.rs:48-52`) describes the
fast-fail behavior the federation path does not actually have.

**Fix:** wrap each call in `offthread` + the `try_*` variant. The
helper signatures already exist; this is a one-line change per
call site.

### F3. `RuntimeTraceStore::ingest` holds parking_lot mutex across user resolver
**File:** `src/server/runtime_trace/store.rs:147`
`ingest()` calls the user-supplied resolver under the parking_lot
`Mutex` that protects the store. `federation_resolver` walks
`fed.list_repos()` and does `find_node_by_name` per registered
repo — under heavy ingest this serialises every snapshot /
edges_from / edges_to / purge_expired call behind a potentially
slow resolver. Any future caller of the resolver that needs the
same lock held by a caller of `ingest` would deadlock
deterministically.

**Fix:** pre-compute parent-resolution pairs outside the lock, then
acquire the lock to write.

### F4. `lain repos add/list` silently destroys corrupt `repos.yaml`
**File:** `src/cli/repos.rs:40, 61`
`FederationConfig::load(config_path).unwrap_or_default()` swallows
load errors. If `repos.yaml` exists but is unreadable or invalid
YAML, both functions treat it as empty. `add` then writes a
brand-new file over the corrupt one — the operator's existing
configuration is lost with no error message.

**Fix:** if the path exists and the load fails, propagate the error
(see `workspaces.rs:113-119` for the pattern that does it right).

## P1 — silent correctness

### F5. `assess_change` miscounts callers (broken `extract_section` + `count_bullets`)
**File:** `src/server/tools/handlers/semantic.rs:523-547, 549-562, 297-334`
`extract_section` scans for `## ` markdown headers. The only
callers feed it raw `get_call_sites` (line-prefix bullets) and
`get_blast_radius` output, neither of which has `## ` headers.
`in_section` stays false; the fallback "first 20 non-empty lines"
fires for every call.

Downstream: `count_bullets` is fed the same body but only counts
lines starting with `  - ` / `- - `. `get_call_sites` lines start
with `- `, so `direct_count` is **always zero**.
`get_blast_radius` direct bullets also start with `  - `, so
`transitive_count` lumps direct + indirect together. Risk verdict
is therefore driven by the combined count, not direct vs
transitive separately. Existing tests pass only because they use
≤1 direct caller per case.

**Fix:** rewrite `extract_section` to actually parse the body
format callers produce, or restructure `assess_change` to call
`count_bullets` against the section the function is meant to
extract.

### F6. `overlay::merge` orphans nodes on duplicate IDs
**File:** `src/server/overlay.rs:484-514`
`graph.add_node(node.clone())` always allocates a fresh `NodeIndex`
and `index_map.insert(node.id.clone(), new_idx)` overwrites any
existing entry. When `other` carries a node whose id is already in
`self`, the previously-mapped node is left in the graph but no
longer reachable by id — `remove_node(original_id)` won't clean it
up (because `original_id` now points to the new index). Merging
corrupted/overlapping overlays silently grows the in-memory graph.

**Fix:** detect duplicate ids in `merge` and skip the insert (or
overwrite the existing node weight under the same `NodeIndex`).

### F7. `start_watcher` check-then-set race on `self.watcher`
**File:** `src/server/federation/repo_index.rs:720-722, 888-889`
Guard reads `self.watcher.lock().is_some()` under its own lock and
releases it before reaching `*self.watcher.lock() = Some(...)`.
Two concurrent `start_watcher` callers can both pass the gate
and each install a `RecommendedWatcher` plus a `JoinHandle`. The
second install overwrites the first watcher (dropping its inotify
subscription) without aborting it, leaving the first receiver
task running and racing the second against the same
`index_lock`.

**Fix:** hold `self.watcher` and `self.watcher_task` together
(either as a combined-state struct under one Mutex, or via a
single guard over both fields) across the full install.

### F8. `run_forget` skips the reload signal
**File:** `src/cli/workspaces.rs:383-394`
After `lain workspaces forget <name>` the running server doesn't
pick up the change until the config-file watcher fires on its own
schedule. The sibling `run_remove` (line 200-203) does call
`signal_reload`. Symmetry fix.

## P2 — defensive / error handling

### F9. `MCP handler::StatusCode::from_u16(http_status()).unwrap()` can panic
**File:** `src/server/mcp/handler.rs:1563`
Auth rejection builds its status via
`StatusCode::from_u16(...).unwrap()`. If `AuthFailure::http_status()`
ever returns an out-of-range code (>999) the handler panics,
turning an auth rejection into a 500 with no recovery. The
sibling `/hook/evaluate` branch already uses
`unwrap_or(StatusCode::UNAUTHORIZED)` (line 2023); mirror that.

### F10. `tools::get_job_status` returns empty string on serialization failure
**File:** `src/server/tools.rs:506`
`serde_json::to_string(job).unwrap_or_default()` reports `Ok("")`
with `success: true` on serialization failure. Agents can't
distinguish "unreadable job snapshot" from a real empty result.

**Fix:** return `LainError` on serialization failure; let the
handler translate to a structured error response.

### F11. `tools::install_language_servers` swallows `get_all_tracked_files` errors
**File:** `src/server/tools.rs:998`
`g.get_all_tracked_files().unwrap_or_default()` treats any
failure as "no tracked files". On a partially-broken git state
(locked index, missing work-tree) the user gets an empty install
response with no hint why.

### F12. `offthread` "abort" doesn't actually stop the blocking thread
**File:** `src/server/ingest/blocking.rs:41-42, 78, 90`
The doc comment claims "The closure runs to completion or is
aborted". `JoinHandle::abort()` on a `spawn_blocking` task only
signals cancellation — the actual sync closure (libgit2 revwalk,
ONNX inference, file reads) keeps running on the blocking pool
until it returns naturally. Every `offthread` call site
(`ingestion.rs:58, 93, 103, 606, 690, 757, 810`) and the bare
`spawn_blocking` calls in `repo_source.rs:205, 333` and
`workspace.rs:313` leak that work on cancel. Repeated
cancellations during shutdown can saturate the blocking thread
pool.

**Fix:** doc the actual behaviour ("cancellation is cooperative;
sync work continues until the closure returns"). For callers
that need real cancellation, use `tokio::select!` against a
cancellation token inside the blocking closure.

### F13. Join-loop cancel/timeout check is after the await
**File:** `src/server/ingest/ingestion.rs:395-411` (and federation analog 1326-1330)
`while let Some(res) = set.join_next().await { if cancel... }` waits
on the next batch result *before* observing cancel or scan
timeout. A single slow batch blocks cancel/timeout detection
until that batch finishes. The timeout governs inter-batch
waiting, not in-flight work.

**Fix:** poll `cancel.is_cancelled()` and the timeout deadline
*before* each `set.join_next().await` via `tokio::select!`.

### F14. Federation placeholder-insert error silently swallowed
**File:** `src/server/federation/federated_index.rs:494-498`
`let _ = self.backend.upsert_node_global(...);` discards any
`LainError`. The companion code unconditionally inserts the `gid`
into `placeholder_ids` and pushes edges whose target the backend
just rejected. Edges become orphaned in the federated backend
with no signal.

**Fix:** on `Err`, skip `placeholder_ids.insert` so subsequent
edges get filtered the same way as an unknown repo.

### F15. Federation `project_repo` holds `repos.read()` across unbounded `nodes()` walk
**File:** `src/server/federation/federated_index.rs:564-575`
`project_repo` acquires `projection_lock` and then holds
`self.repos.read()` for the entire duration of `other_nodes`
construction (which calls `idx.nodes()` / `db.all_nodes()` per
peer repo). For the lain repo's ~3K-node scale this is OK; for
larger federations it's a writer-starvation hazard.

**Fix:** snapshot the `(RepoId, Arc<RepoIndex>)` pairs first, drop
the read guard, then walk them.

### F16. Federation `rebuild_symbol_index` holds `projection_lock` through O(N×M) work
**File:** `src/server/federation/federated_index.rs:713-744`
Same shape as F15. `add_repo` / `remove_repo` both need the
projection_lock; `project_repo` holds it through a per-repo walk
of every node. Add a docstring on `projection_lock` documenting
the "read-only fast-path" expectation so a future contributor
doesn't plumb an `.await` into `nodes()`.

### F17. Federation `content_hash` failure path differs between loader and runtime persist
**Files:** `src/server/federation/loader.rs:212-216`,
`src/server/federation/federated_index.rs:157-164`
The loader propagates `content_hash` `Err` as `LainError::Git`,
aborting the whole manifest save. The federation's runtime
`persist_manifest` matches on it as `continue` (silently drops the
entire repo entry). The two paths produce different on-disk
snapshots for the same source state. Pick one and document it.

### F18. Federation `upsert_nodes_batch` partial-failure leaves in-memory state
**File:** `src/server/federation/graph_backend.rs:145-165`
The "single save at the end" optimization presumes all-or-nothing,
but the `?` inside the loop returns `Err` the moment any inner
`db.upsert_node` / `db.upsert_edge` fails, after the prior N-1
iterations have already mutated `self.db` and (for nodes)
`self.index`. No rollback, no record of which writes committed.

**Fix:** either wrap in a transaction-like staged buffer the
backend exposes, or document the partial-state semantics and
add a test that locks the on-disk equivalence.

### F19. `audit::rotate_if_full` TOCTOU on rotation check
**File:** `src/server/audit.rs:144-150`
`path.exists()` then `metadata()?.len()` then `rename(&path, &rotated)`
are three separate syscalls with no lock between them. If the
file is removed between `exists()` and `metadata()`, the `?`
propagates an `io::Error` and the event is dropped on the floor.

**Fix:** drop the `exists()` precheck — `metadata` returns 0 size
on `NotFound`, which already skips rotation cleanly.

### F20. `audit::replay` silently skips malformed JSONL
**File:** `src/server/audit.rs:202`
`BufReader::lines().map_while(Result::ok)` treats any per-line parse
error as a dropped event with no logging. Under a torn-line
scenario the audit trail loses entries with no observability.

**Fix:** at minimum log the malformed-line count at `tracing::warn!`
so an operator notices a silent audit gap.

### F21. `sensors::websocket_sensor` mixed-count metric
**File:** `src/server/sensors/websocket_sensor.rs:104-118`
`count` is incremented once per inserted edge *and* once per
unique URL on the same loop iteration. Other sensors count nodes
or edges uniformly; this one returns a sum that's neither. Not a
panic, but a caller of `SensorCounts::websocket` will get an
answer that doesn't mean what the field name implies.

**Fix:** track two counters or pick one (likely "edges minted") and
use it consistently.

### F22. `tools::register_job_webhook` accepts and persists empty URL
**File:** `src/server/tools.rs:490-499`
No validation of `url`; empty string is allowed into the webhook
list and will later be POSTed to as an empty URL.

**Fix:** reject empty / unparseable URL at registration.

### F23. `assess_change` emits "Untested dependents" section even when `include_tests: false`
**File:** `src/server/tools/handlers/semantic.rs:297-311`
When `include_tests` is false the code returns `Ok(String::new())`
and `trim_for_section("", 8)` falls through to `"(none)"`. The
user asked to omit the section; the handler prints a misleading
heading with no content.

**Fix:** skip the heading entirely when the section is empty.

### F24. `enqueue_caller` drops unresolvable callers silently
**File:** `src/server/tools/handlers/impact.rs:151-154`
When `caller` is `None` (source_id not in graph) the closure
pushes the id into the BFS queue without recording it in
`affected_names`/`session_nodes`. Stale edges whose endpoint
vanished from the index are still walked (correct for transitive
discovery) but their existence isn't reflected in the
`visited` count or the headline total.

**Fix:** count dropped callers in a separate metric.

### F25. `tools::run_clippy` constructs `Command` manually — skips toolchain PATH prepend
**File:** `src/server/tools/handlers/execution.rs:302-313`
`run_clippy` builds the `Command` by hand instead of going
through `parse_command` (which prepends the toolchain dir so
`cargo` finds `rustc`). When the binary was launched with a
toolchain-free PATH, `rustc` won't be found by the child and
clippy fails with "could not execute process rustc -vV".

**Fix:** route through `parse_command`.

### F26. Federation `parse_node_type` silently substitutes `Function` for unknown kinds
**File:** `src/server/federation/federated_index.rs:758-782`
`_ => N::Function` fallback means a corrupt or forward-compat
global-id with an unknown kind materializes a `Function`
placeholder. Until `project_repo` runs, `search_org` queries
against the placeholder return a `Function` hit for what may not
be one.

**Fix:** `tracing::warn!` with the unknown kind string.

### F27. Federation `repo_index` cycle gate collapses `Err(_)` to `now.duration_since`
**File:** `src/server/federation/repo_index.rs:628-630`
`now.duration_since(last).unwrap_or(Duration::ZERO)` collapses a
backwards clock to 0, arming the suppression branch. If
`now < last` (NTP correction, monotonicity violation), the
suppression path runs against possibly-bizarre `git` and `db`
reads. Explicitly log and skip instead of silently going
through suppression.

### F28. `static LOCK: Mutex<()>` declared inside a `#[test]` function
**File:** `src/cli/doctor.rs:820-821`
The `static` is created fresh on every test run. cargo test runs
tests in parallel by default; concurrent tests mutating
`LAIN_LSP_PREWARM` race. Move the `static` to module scope.

### F29. `hooks::observe` has duplicate code paths
**File:** `src/cli/hooks.rs:666-685`
The `if !at.is_empty()` and `else` arms are byte-for-byte identical
HTTP POSTs. Future fixes won't propagate. Unify.

### F30. `hooks::observe` swallows the HTTP response silently
**File:** `src/cli/hooks.rs:677, 684`
`let _ = resp; // fail-open` — the agent-visible activity feed
silently drops server-side failures. Acceptable per the design,
but `eprintln!`ing the status code on non-2xx would help diagnose
a misconfigured endpoint.

### F31. `resolve_pattern_edges` `edges_per_value` is dead config
**File:** `src/server/ingest/resolve.rs:264, 309`
The doc-comment contract is "capped by `max_edges` overall and
`edges_per_value` per value", but the only enforcement is
`let max_edges = (scored.len() * limits.edges_per_value).min(limits.max_edges);`
The inner loop at 313-353 enforces only `edges.len() >= max_edges` —
never a per-value limit. Setting `edges_per_value = 2` does not
cap any single value's emitted pairs to 2; it just lowers the
global budget.

**Fix:** add a per-value cap in the inner loop, or update the
doc-comment to match what the code does.

## P3 — code quality / dead code

### F32. `tests::doctor::LOCK` declared inside `#[test]` (see F28)
### F33. `mcp::handler` UI handlers have unreachable `None` arms
**File:** `src/server/mcp/handler.rs:2043-2051, 2084-2092, 2125-2133`
The three UI handlers guard the entry with `path.starts_with(...)`
and then `match path.strip_prefix(...)`, expecting a `None`
branch. Because the prefix is verified, the `None` arm is
unreachable. Use `let session_id = &path[..prefix.len()]` or a
single `unwrap()`.

### F34. `annotations.rs` redundant `strip_prefix("edge:")`
**File:** `src/server/annotations.rs:497-499`
For `target_kind == "edge"`, the first strip removed `"edge:"`,
so `stripped` no longer starts with `"edge:"` and the second
`strip_prefix("edge:")` always falls through to `unwrap_or(stripped)`.
Inline `payload = stripped`.

### F35. `tests::runtime_trace::handle.abort()` called twice
**File:** `src/server/runtime_trace/server.rs:337-339`
Second abort is a no-op. Cosmetic.

## Performance upgrades (not bugs)

### U1. `search.rs::semantic_search` holds cache lock across NLP forward pass
**File:** `src/server/tools/handlers/search.rs:85-149`
`embedding_cache` is taken once outside the per-node loop and
held for the full N-node iteration, including `embedder.embed()`
and `graph.upsert_node()`. Release the lock per-node, or split
into read-only and write phases.

### U2. `mcp::handler` recomputes `static_graph_generation_unix` per call
**File:** `src/server/mcp/handler.rs:593-596, 614`
Reach into parking_lot or read state on every `handle_call_tool_request`.
Cheap individually but called per request. Fold into a per-server
cache if profiling shows contention.

### U3. `mcp::handler` calls `srv.auth_handle_inner().auth()` twice per request
**File:** `src/server/mcp/handler.rs:1555, 1567**
Both `check_bearer` and `check_rate` go through the same call;
each request locks and unlocks twice. Cache the handle in a
local.

### U4. `runtime_trace::store` clones the entire snapshot for `edges_from` / `edges_to`
**File:** `src/server/runtime_trace/store.rs:222-234`
Every query allocates and clones every `RuntimeEdge` into a `Vec`,
then filters. For 100k-edge stores this is wasteful.

### U5. `presence::compute_symbol_hash` reads entire file
**File:** `src/server/presence.rs:2319`
For large files, `std::fs::read` allocates the whole file. Could
use `blake3::Hasher` streaming via a `BufReader`. Source files
are typically small, so the win is marginal — flag for
profiling-driven optimization.

### U6. `mcp::handler` UI handlers hold tokio::Mutex across HTML build
**File:** `src/server/mcp/handler.rs:2052, 2093, 2134`
`executor.ui_sessions().lock().await` returns a `tokio::Mutex`
guard held for the entire HTML-substitution-and-response-build
body. Clone the `(symbol, nodes)` triple out of the lock before
mutating the HTML.

### U7. `cli::mcp_endpoint` substring stripping can over-trim
**File:** `src/cli/hooks.rs:472-481`
`endpoint.trim_end_matches("/mcp")` repeatedly strips any
trailing `/mcp`-shaped substring. Not a hot bug for the shipped
`http://host:port` shapes, but worth verifying.

### U8. `sensors::dynamic_dispatch_sensor` aborts scan on first error while siblings continue
**File:** `src/server/sensors/dynamic_dispatch_sensor.rs:268-296`
The four sensors use inconsistent failure policies. Pick one
across all five.

## Recommended PR queue (priority order)

A focused PR per P0/P1; bundle related P2s; leave P3s and upgrades
for opportunistic cleanup.

| PR | Severity | Items | Branch name |
|----|----------|-------|--------------|
| **1** | P0 | F1, F11, F12 (timeout orphans + offthread docs) | `fix/tool-timeout-orphans` |
| **2** | P0 | F2 (federation not fail-fast on git mutex) | `fix/federation-git-mutex-failfast` |
| **3** | P0 | F3 (RuntimeTraceStore mutex across resolver) | `fix/runtime-trace-resolver-lock` |
| **4** | P0 | F4 (repos add/list destroy corrupt yaml) | `fix/repos-add-corrupt-yaml` |
| **5** | P1 | F5 (assess_change miscount) | `fix/assess-change-miscount` |
| **6** | P1 | F6 (overlay merge leaks), F7 (start_watcher race), F8 (run_forget signal), F14 (placeholder error), F15/F16 (federation lock holding) | `fix/federation-concurrency-and-cleanup` |
| **7** | P2 | F9–F31 (error-handling, defensive, code quality — single PR or split by file) | `fix/error-handling-cleanup` |
| **8** | perf | U1–U8 | `perf/handler-and-trace-hotpaths` |

## Testing strategy per PR

- **F1 (tool timeout):** test that `cmd.kill_on_drop(true)` is set
  on the three `Command`s; integration test that a 1 s timeout
  on a long-running cargo actually kills the child (use a sleep
  command in a controlled fixture).
- **F2 (federation fail-fast):** unit test that
  `git.get_all_tracked_files()` is called via the `try_*` variant;
  property test that a wedged mutex unblocks within the watchdog
  threshold.
- **F3 (RuntimeTraceStore):** test that `ingest` completes
  successfully with a slow resolver that exceeds the inner work
  window.
- **F4 (corrupt yaml):** test that a `repos.yaml` containing
  invalid YAML produces a typed error and never overwrites the
  file.
- **F5 (assess_change):** regression test that asserts the direct
  count for a known fixture is correctly > 0 and the risk verdict
  uses direct vs transitive separately.
- **F6 (overlay merge):** test that merging overlapping overlays
  keeps the original NodeIndex for the shared id.
- **F7 (start_watcher race):** test that two concurrent
  `start_watcher` calls do not leak a second receiver task.
- **F8 (run_forget signal):** test that `run_forget` triggers
  `signal_reload`.
- **F11/F12 (offthread docs):** update docstrings; add a test
  that `offthread(cancelled).await` returns within the deadline
  even when the closure would have run for seconds.

## Notes

- Findings F1–F4 are P0 reliability bugs that should not merge
  to `dev` before landing fixes. F1 alone is enough to cause
  user-facing "agent call hangs forever" reports.
- The total scope is ~30 fixes across 8 PRs. Each PR is bounded
  enough to review in under an hour and ships with its own
  regression tests.
- All fixes preserve the stability boundary (presence, state_lock,
  federation CrossRepoResolver, ONNX session, watcher, cancel
  token).
- This document supersedes the earlier per-PR audit reports; any
  finding already fixed in PRs #206-#211 is omitted from the list
  above (the `load_from_disk` rebuild, the `LspPool::Clone`
  round-robin, the watcher lifecycle handles, the FS-I/O mutex
  release, the multi-symbol hash, the LSP visibility warnings,
  the federation overlay re-check, the watcher init failure
  handling, and the `expire_stale` single-lock pattern).
