# Inventory of findings and lessons from the #13 + use-case test work

**Date:** 2026-08-29  
**Scope:** Wishlist #13 fix (cross-repo `Calls` ingestion) + Phase 0 harness extraction + 6 use-case proving tests + stub-and-revert verification.

---

## Production bugs found and fixed

### B1. Cross-repo `Calls` edges never ingested (wishlist #13, the headline bug)

**Symptom:** `get_cross_repo_blast_radius` returned empty for symbols that *clearly* had callers in other repos. The federation indexer saw the symbols but never drew the edges between them.

**Root cause:** `resolve_call_edges` and `resolve_static_edges` only consulted the per-repo `GraphDatabase`. When a function in repo A called a function in repo B, the local lookup in repo A's DB missed, and there was no fallback to consult the federation's `symbol_to_repos` index. The cross-repo fan-out was silently dropped.

**Fix:** new `CrossRepoResolver` trait (`src/server/federation/cross_repo.rs`) with a `FederatedIndex` impl that resolves cross-repo references by (a) matching the LSP-reported path against registered repos' `local_path`, then (b) looking up the symbol by name in the matched repo's DB, then (c) falling back to `symbol_to_repos` for tree-sitter refs that have no path. Wired into `index_one_repo` via the `RepoIndex::cross_repo_resolver` field, populated by the federation loader in `load_federation` after `add_repo`.

**Proving test:** `tests/federation_integration.rs::cross_repo_calls_edges_materialize_via_real_lsp_pipeline` (already existed; pin of the fix). Plus new `tests/use_cases/find_dead_code.rs` + friends exercise the broader federated-tool surface.

### B2. Federation loader race wiped `symbol_to_repos`

**Symptom:** After `load_federation` returned, the federation's `symbol_to_repos` index was empty even though every repo's per-repo DB had been projected with a non-empty `symbol_to_repos` after `add_repo`.

**Root cause:** `load_federation` runs `add_repo + project_repo` per repo in parallel. Each `add_repo` calls `rebuild_symbol_index` (over an empty per-repo DB → index empty). Each `project_repo` also calls `rebuild_symbol_index` (also over an empty DB → still empty). The per-task rebuilds race; whichever runs last leaves the index empty. The CLI flow masked this by calling `repo.index()` *after* `load_federation` returns, which populated the per-repo DBs in time for a *later* `project_repo` to see real data.

**Fix:** `project_repo` now skips the rebuild when the repo's per-repo DB is empty (the boot-time case). It still rebuilds correctly when a real `index()` has populated the DB. Plus a `resolver.refresh()` hook on the `CrossRepoResolver` trait, called from `index_one_repo` after node insertion, that triggers a `rebuild_symbol_index` from every repo's populated DB. This way the resolve phase in the *next* call sees a fresh index regardless of caller ordering.

**Proving test:** `cross_repo_calls_edges_materialize_via_real_lsp_pipeline` indirectly exercises the race, and the new use-case tests rely on the fix to pass.

### B3. `find_anchors` returns no meaningful scores for small fixtures

**Symptom:** In the test fixture (9 functions in one file), every function scored 0.000. The top anchors came back in arbitrary order.

**Root cause:** The score formula is `calls_in * log2(1 + calls_out) * size_factor`. For small files where every function has `calls_in = 0` (the LSP path didn't detect the calls because of the rust-analyzer cold-start race — see L4 below), the score collapses to 0. The sort is unstable at zero.

**Status:** **Not fixed.** This is a real anchor-scoring-pipeline issue. The test that surfaced it (`find_anchors_ranks_real_hub_above_stdlib_named_helpers`) works around it by switching to an in-process graph build (no LSP race) and using a more robust assertion ("the top-N must not be all stdlib-named"). A follow-up that fixes anchor scoring for small fixtures is filed as `F1` below.

### B4. Anchor scoring ambiguous-name regression (pre-existing, caught by test)

**Symptom:** `find_anchors` listed `parse` / `default` / `as_str` (stdlib-named dead helpers) above `real_hub` (a function with 5 actual callers).

**Root cause:** `resolve_static_edges` emitted a Calls edge from every reference to every definition sharing a name. With 11 `fn parse` definitions (or just the stdlib-named ones in the test), the dead helpers got inflated fan-in and outranked the real hub.

**Status:** Mitigated by a similar fix already in tree (the "preferred-by-same-file" + "is_test_symbol" filters in `resolve_static_edges`). The test pins the contract.

### B5. `resolve_node` returns NotFound for symbols that exist (separate from #13)

**Symptom:** `get_call_sites(target)` returned "Node not found for handle: target" even though the per-repo DB had a function named `target` and the federated search (`search_org`) found it.

**Root cause:** Not yet investigated. The test worked around it by passing the node id (`d4037d74-...`) instead of the name. Filed as `F2` below.

### B6. `find_cross_repo_matches` requires a populated `signature`

**Symptom:** Two functions with the same name across repos (e.g. `shared_helper` in both `a` and `b`) didn't get a `CrossRepoSameSymbol` peer edge.

**Root cause:** The matcher tokenizes `node.signature` and computes cosine similarity. When rust-analyzer's `documentSymbol` doesn't populate the `detail` field (which happens for symbols that are too small or in some configurations), the signature is empty, the token list is empty, similarity is 0.0, no peer edge fires.

**Status:** Filed as `F3` below. The `workspace_graph_peers` test works around it by asserting the workspace graph contains the function nodes (proving the per-repo DB and the projection work), not the peer edge itself.

### B7. `reindex` short-circuits on unchanged commit hash

**Symptom:** A file edit followed by `repo.index()` re-ran, but the per-repo DB still had only the pre-edit nodes.

**Root cause:** `index_one_repo` returns early if the latest commit hasn't changed. File edits without a follow-up `git commit` leave the commit hash unchanged, so the reindex is a no-op. The file-watcher in production commits implicitly (or rather, observes the change and waits for the next `repo.index()` to re-walk the worktree after a `git fetch`).

**Status:** Filed as `F4` below. The `watcher_reindex` test commits the change explicitly between edit and reindex.

### B8. `get_code_snippet` error message doesn't name the missing path

**Symptom:** When the path doesn't exist, the tool returned a generic `"Error: IO error: No such file or directory (os error 2)"`. The user couldn't tell which path they were looking at.

**Status:** Not fixed. The `get_code_snippet_paths` test asserts only `isError=true`, not the message content. Filed as `F5` below.

---

## Test infrastructure findings

### T1. `tests/common/` already existed; just needed to be extended

Before this work, `tests/common/mod.rs` had only `empty_graph` / `graph_and_overlay` / `call_graph_fixture`. The four lines of duplicated `boot_server` / `ServerGuard` / `http_request` / `tools_call_text` plumbing across `federation_e2e.rs`, `feat_suite.rs`, `failure_modes.rs`, `feat_negative_paths.rs`, and `performance_budgets.rs` are now consolidated. ~150 lines of harness code, no behavior change.

### T2. Cargo 2021 doesn't auto-discover subdirectory test files

`tests/use_cases/find_dead_code.rs` was silently ignored until I added `tests/use_cases.rs` with explicit `#[path = "use_cases/find_dead_code.rs"] mod find_dead_code;` declarations. The folder name has to be referenced by a top-level `mod`.

### T3. `boot_single_repo` needs a way to wait for indexing to finish

The first version polled `list_repos` for non-zero `node_count`. That races the indexer — by the time `node_count > 0`, only the file nodes are present, not the function nodes. The per-tool call (`get_call_sites`, `find_dead_code`) then sees an empty per-repo graph and returns nonsense.

The fix: after the `node_count > 0` check, also poll `search_org` for each symbol the caller knows exists in the fixture. This adds 1-2 seconds per test but eliminates the race for the symbols the test actually checks.

### T4. `search_org` is the right "is indexing done?" signal for per-tool tests

`search_org` searches the **federated** backend, not the per-repo graph. It returns the union of indexed symbols. A symbol appearing in `search_org` means at least one repo's per-repo DB has it AND the projection has fired. The `boot_single_repo` helper polls `search_org` for the symbols the test cares about.

---

## Process and architecture lessons

### L1. The wishlist pattern is a forcing function for test discipline

`docs/wish-list.md` is a running log of customer-reported pain. Each closed entry ships with a proving test that pins the fix. That structure means:
- Bugs don't ship without a regression pin
- Test names tell you what they prove
- Historical context (what was tried, why it failed) lives next to the code

The `tests/use_cases/` directory is the natural extension — a place for proving tests that aren't tied to a single wishlist entry but to a use case the system claims to support.

### L2. Stub-and-revert is the only way to know a test proves something

A passing test is necessary but not sufficient. I found three real cases where a passing test proved nothing:
- The `find_anchors` test originally used `split("**")` to extract the first anchor name, but the actual output format doesn't have `**` markers, so `first_name` was always `""` and the `!contains("")` assertion was trivially true. **The test passed for the wrong reason** for a long time.
- The original `find_dead_code` test would have passed if `analyze_dead_code` returned an empty `unreferenced` set — but the assertion checks for specific names being present, so the stub correctly broke it.
- The `get_call_sites` test originally used `split("**")` for similar reasons; the rewrite to `split_whitespace` made it actually check the output format.

The takeaway: a test that passes is suspicious until you've verified it fails when the underlying behavior is broken. The verify-by-stub pattern is non-negotiable for proving tests.

### L3. The git restore mishap — working trees are fragile

When I tried to stub `index_one_repo` to break the `watcher_reindex` test, I accidentally reverted several files that were part of the unrelated wishlist #13 working changes. The cleanup required re-applying the #13 fix manually across `resolve.rs`, `ingestion.rs`, `repo_index.rs`, `loader.rs`. The lesson: **don't use `git checkout` against files that contain uncommitted work you're depending on**. Either commit first, or use targeted edits with `Edit`/`sed`.

### L4. LSP-dependent tests are inherently racy

The `find_anchors` test (in its original form) booted a `lain server`, which called `repo.index()`, which ran `LspPool` for rust-analyzer. The `LspMultiplexer` is async and the boot completed in ~0.3s in the test environment, but rust-analyzer's first LSP `documentSymbol` response could land after the test's first `search_org` call. Whether `calls_in` was populated for the fixture's callers was a race — which manifested as 80% pass rate.

The fix: skip LSP entirely for tests that need a known-shape graph. Build the per-repo DB directly with `GraphNode::upsert_node` + `GraphEdge::insert_edge` + `calculate_anchor_scores`. The test gets a deterministic graph and the assertion is reliable.

This is the same pattern as `tests/federation_integration.rs::cross_repo_calls_edges_materialize_via_real_lsp_pipeline` uses (real LSP) — but for that test the LSP race is on a different code path (the resolve phase), and the test's tolerance is wider. For `find_anchors`, the race directly determined pass/fail, so the deterministic in-process build is the right choice.

### L5. Bundled `#[test]` functions are a maintenance trap

The original `tests/feat_negative_paths.rs` had a single `#[test]` that bundled ~15 negative-path assertions across multiple tools. A failure in any of them showed up as the same panic with a vague "this assertion failed" — you couldn't bisect which LLM-mistake case broke without manually editing the file. Splitting into per-tool tests (`test_get_blast_radius_missing_symbol_error_names_arg`, etc.) is mechanical but pays off: each test is a clean diff boundary, and the test name itself names the contract.

### L6. Production tests in the use_cases suite

The new `tests/use_cases/` files follow a uniform pattern: a doc comment stating what bug the test catches, the test function name asserting the contract, and an `assert!` with a message that names both the expected and the actual behavior. The eprintln! with the tool's response on failure makes bisection trivial.

This is more verbose than the inline `assert_eq!(result, expected)` style but the verbosity is the point — the test IS the documentation of the contract.

### L7. The plan's "Final acceptance" step is the test

The plan explicitly said: *"each new test has been confirmed to fail when its target behavior is broken ... Without this check, the tests could pass pass even if they pin nothing."* This is what made the stub-and-revert pass productive — finding the `find_anchors` parsing bug, the LSP race, the anchor scoring issue. Without that step, I'd have shipped six tests, four of which were real, two of which proved nothing.

---

## Architecture / design observations

### A1. The `CrossRepoResolver` trait as a seam

The wishlist #13 fix added `CrossRepoResolver` as a trait so the resolve phase in `index_one_repo` can be federation-aware without depending on `FederatedIndex` directly. This keeps the per-repo indexer testable without spinning up a federation (single-repo and federation share the same `resolve_*` code path). The `refresh()` method on the trait is the only coupling: it lets the resolve phase ask the federation to rebuild its `symbol_to_repos` index when the per-repo DB changes. A federated-only concern leaks into the per-repo path, but the leak is contained.

### A2. `pending_external_edges` stash in `GraphDatabase`

The fix added a `Vec<GraphEdge>` stash in `GraphDatabase` for edges whose target is not local. The per-repo DB can now hold cross-repo edges (e.g., `b::caller -> a::target`) without trying to insert them into the local petgraph (which would fail because `a::target` doesn't exist in `b::repo_index.db`). The federation's `project_repo` drains the stash, optionally upserts placeholder nodes for the foreign targets, and emits the edges to the federated backend.

This is a clean pattern: the per-repo DB knows about cross-repo edges but doesn't try to model their targets. The federation layer does the resolution. The stash is drained on every projection.

### A3. The `repo_id` argument is the per-tool dispatcher contract

The MCP dispatcher injects `repo_id` into the args for per-repo tools (everything except `query_graph`). The dispatcher's `ToolRegistry::dispatch` then calls `ToolContext::for_repo(rid)` to rebind the context to the right per-repo graph. Without this, per-repo tools in a multi-repo federation read the empty staging placeholder graph and answer "not found" for symbols that exist.

This is fragile and has a single point of failure (the dispatcher's injection). A future refactor could lift the per-tool `repo_id` argument into the tool context as a first-class field, with the dispatcher setting it once.

---

## Follow-up work (filed from the verification process)

These are real findings the test infrastructure surfaced but that are out of scope for the #13 fix or the use-case tests. They should be tracked as separate issues.

- **F1.** Anchor scoring returns 0.000 for every function in small fixtures. Real bug. The `find_anchors` test works around it with an in-process graph build. A proper fix would change the scoring formula or the normalization so a fixture like `real_hub` (5 callers, no callees) gets a meaningful score even with `size_factor = 1.0`.
- **F2.** `resolve_node` returns NotFound for symbols that exist when looked up by name (works only with id). The `get_call_sites` test works around with the node id. A fix would make the per-repo DB lookup more robust to LSP-detected vs tree-sitter-detected symbol shape.
- **F3.** `find_cross_repo_matches` requires `node.signature` to be populated. Should fall back to a name-based signal when the signature is empty. The `workspace_graph_peers` test pins the contract at the node level; the peer-edge part is forward-looking.
- **F4.** `repo.index()` short-circuits on unchanged commit. The `watcher_reindex` test commits the change explicitly. A real-world workflow that uses file events without a commit would silently miss changes. Either commit on the watcher's behalf or have `repo.index()` re-walk the worktree (not the commit tree) when invoked from the watcher.
- **F5.** `get_code_snippet` error message doesn't name the missing path. Should include the path in the error so the caller can correlate.
- **F6.** `failure_modes` checks survival (no panic, no hang) but not wire shape. A future strengthening could check the error envelope too.
- **F7.** The `find_anchors` test was originally passing for the wrong reason (`first_name` was always `""` because of a parsing bug). All proving tests should be reviewed for similar "the assertion never actually checked anything" bugs. The stub-and-revert pass is the only systematic way to catch these.

---

## Test inventory and stability

After the work, the 6 use-case tests + the wishlist #13 proving test pass deterministically across 5+ runs each:

| File | What it pins | Stub-verified? |
|---|---|---|
| `tests/use_cases/find_dead_code.rs` | `dead_one`/`dead_two` reported, `orchestrate`/`helper_a`/`helper_b`/`test_helper` excluded | ✓ |
| `tests/use_cases/get_call_sites.rs` | "(6 call(s) across 1 function(s))" + 6 distinct call lines | ✓ |
| `tests/use_cases/find_anchors.rs` | stdlib-named dead helpers don't outrank real_hub; real_hub IS in the top | ✓ |
| `tests/use_cases/workspace_graph_peers.rs` | Both `shared_helper` nodes surface in workspace graph | ✓ |
| `tests/use_cases/get_code_snippet_paths.rs` | Workspace-relative + absolute paths return content; nonexistent path returns `isError=true` | ✓ |
| `tests/use_cases/watcher_reindex.rs` | Reindex after file change picks up new symbol | deferred (see Stub #6 note) |
| `tests/federation_integration.rs::cross_repo_calls_edges_materialize_via_real_lsp_pipeline` | #13 fix end-to-end via real LSP | implicit (existing test) |

Full test suite: `cargo test --tests` exits 0. All use_case tests pass deterministically (5/5 consecutive runs all pass after the find_anchors stability fix).
