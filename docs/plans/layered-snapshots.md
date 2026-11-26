# Plan: share identical repos between snapshots, and analyse a PR interface-first

> Handed off from a Parallax session on 2026-11-26. Saved verbatim (paths
> adjusted to the real tree: snapshot code lives under
> `src/server/federation/contracts/`). Decisions and measurements are
> appended under "Phase 0 findings" as they land.

**Repo:** Lain (`spuentesp/lain`), base `origin/dev` (v0.16.2 + post-release). Parallax changes are small and come last.
**Goal:** a PR analysis loads only what it has to: the changed repository at both ends, the cross-repo *interfaces*, and
then only the consumer code that an interface actually reaches. Nothing identical is held twice.
**Why now:** measured 2026-11-26 on the deployed Fly image (local podman, 4 GB RAM + swap):
NVIDIA/OpenShell #4995 peaks at **5.6 GB**, a second PR right after peaks at **6.2 GB**, and memory at rest grows from
**2.6 to 5.1 GB** after two analyses. The Fly machine has 7.9 GB and no swap. Details in parallax `deploy/fly/README.md`.

## How it works today (read these first)

| Step | Where | What it holds |
|---|---|---|
| Per-repo index, by `(repo, commit, analyzer_version)` | `contracts/index_cache.rs` (`IndexCache`, `read_graph_bytes`) | `graph.bin` on disk. Already shared and already skips re-indexing (0.16.2). |
| Snapshot build | `contracts/snapshots/manager.rs::build_snapshot_federation` (~L1296) | For **every** repo of the snapshot: `hydrate_graph` (a full `GraphDatabase`), then `project_graph` **copies** all nodes and edges, with global ids, into a new `PetgraphBackend::ephemeral`. |
| Contract join | `build_snapshot_contract_index_with_rust_details` (~L1900) | Reads *all* nodes and edges back out of the backend, filters to contract nodes and contract edges, runs `ContractJoiner::run`, then writes `Binds` edges back. |
| Residency | `SnapshotManager` LRU, `LAIN_SNAPSHOT_RESIDENT=2`, `LAIN_SNAPSHOT_RESIDENT_IDLE_SECS=0` (never) | Both ends of the last diff stay resident after the analysis. That is the 2.6 → 5.1 GB growth. |
| Live federation | `federation/federated_index.rs::project_graph` | Its own full projected copy of every repo. |

What Parallax asks for in one analysis (`packages/api/src/worker/assessment-worker.ts`, `stepSnapshot` / `stepAnalyze`):
`prepare_snapshot`/`get_snapshot` for base and head, then `diff_contracts(base, head)`, `get_coverage(head)`,
`trace_impact(snapshot, from: endpoint)` for up to `MAX_TRACE_TARGETS` changed provider endpoints, and `get_service`
for evidence. Code impact (`stepCodeImpact`) is separate.

**So for a PR that touches only OpenShell,** NemoClaw and NeMo-Fabric are at the *same commit* in base and head,
but they are hydrated and projected twice, once per snapshot, plus once more in the live federation if the commit matches.
And all three repositories are loaded in full, even though only the contract surface of the unchanged ones is
needed to know whether the change reaches them.

## Target design

### Part 1: repo layers (share what is identical)

A **`RepoLayer`** is one repository at one commit, projected once: global-id nodes, intra-repo edges, and its
**interface summary** (part 2). It is immutable once built.

- A process-wide **`LayerStore`**: `CacheKey -> Weak<RepoLayer>`. A build looks it up first; a layer lives as long as
  some federation holds an `Arc` to it, and memory comes back when the last holder drops it.
- A snapshot federation becomes **`Vec<Arc<RepoLayer>>` + a small overlay**: the cross-repo `Binds` edges from the
  join, Rust RPC facts (`rust_rpc::resolve`/`attach`), anything else written after projection. Reads go through
  layers then overlay; writes go to the overlay only.
- This needs a read-side composite implementing what the snapshot tools use from `GraphBackend`
  (`get_node`, `visit_nodes`, `all_edges`, neighbours, `query_graph_view_at`). Decide early whether to
  (a) implement `GraphBackend` for a `LayeredBackend`, write methods going to the overlay, or (b) narrow the snapshot
  tools to a smaller read trait. Inventory the calls under `src/server/mcp/contract_tools/` (`view.rs`, `analysis.rs`,
  `evidence.rs`, `contracts.rs`) and `mcp/handler.rs` ~L854 before choosing.
- **Live federation:** sharing layers with live is a later step. The live graph is mutated in place (reconciliation,
  `remove_nodes`, rejoin), so it cannot hand out an immutable layer as-is. Start with snapshot↔snapshot sharing;
  measure; then decide.
- Stop the transient double copy: `hydrate_graph` builds a full `GraphDatabase` only to copy it. Projecting straight
  from the cache bytes into a layer (or keeping the hydrated db *as* the layer with an id-rewrite map) removes it.

Expected effect for a single-repo PR: base + head = 2 × changed repo + 1 × each unchanged repo (instead of 2 × all).

### Part 2: interface-first, depth-second analysis

Do not build the whole federation to map end to end. Divide, and reuse:

1. **Changed repos only, full depth.** Load base and head layers only for repos whose commit differs. Compute the
   changed symbols and the changed *interface*: provided endpoints / RPC methods / schemas / fields added, removed or
   changed, and changed client calls.
2. **Interface summaries for everyone else.** Per `RepoLayer` (and persisted next to `graph.bin` in the cache entry, so
   it is computed once per repo+commit): the contract nodes and contract edges the joiner already filters for
   (`HasField`, `RequestSchema`, `ResponseSchema`, `PayloadSchema`, `ReadsFrom`, nodes with a non-empty `contract`),
   client-call sites, Rust RPC facts, unresolved / dynamic call sites. The join (`ContractJoiner::run`) runs over
   summaries only. It already only looks at that subset, so this should be a change of input source, not of the joiner.
3. **Drill down from the interface.** For each `Binds` edge that lands on a changed contract, load that consumer repo's
   layer **lazily**, trace inward from the bind site (`trace_impact` depth), record the result, release the layer.
   If the consumer itself provides an interface that the impact reaches, continue to *its* consumers (visited set,
   depth cap reported as a gap).
4. A repo whose summary has nothing bound to a changed contract is **not loaded at all**.

**Coverage honesty (non-negotiable, Parallax INV 3: an empty result never means "no impact"):**
- A repo not drilled because no bind reaches it is reported as *reviewed at interface level: no consumer of a changed
  contract*, not as unreviewed and not silently omitted.
- A repo with unresolved, ambiguous or dynamic client calls in its summary must be **either drilled or declared as a
  gap**. Never assumed safe.
- Depth caps, skipped drills and failed layer loads go into the coverage/gaps the tools already return
  (`truncated`, `truncation_reason`, `scope.unreviewed`, `scope.failed`).

Interface-first is a mode, not a replacement: keep the full projection path behind a switch
(e.g. `LAIN_SNAPSHOT_MODE=full|layered`) until the equivalence suite (below) passes on every demo PR.

## Phases (each lands separately, each measured)

0. **Baseline + harness.** Extend `src/bin/measure_snapshot_memory.rs` to report peak and at-rest per phase
   (hydrate, project, join, trace) for a two-PR run. Record current numbers in the PR. Cheap win to try while here:
   default `LAIN_SNAPSHOT_RESIDENT_IDLE_SECS` to something like 120 and check that RSS really comes back (glibc may
   hold freed memory; check `malloc_trim` or an allocator switch only if measurement shows it).
1. **`RepoLayer` + `LayerStore` + snapshot↔snapshot sharing**, full mode semantics unchanged. Gate: equivalence suite
   identical, peak for #4995 lower, unchanged repos built once per process (counted, asserted in a test).
2. **Interface summaries persisted in the cache entry**, join runs on summaries. Gate: identical `ContractIndex` and
   `Binds` for every demo snapshot.
3. **Layered/lazy mode** for `diff_contracts`, `trace_impact`, `get_coverage`, `get_service` on snapshots, with the
   coverage rules above. Gate: equivalence suite; peak and rest memory targets below.
4. **Parallax (small):** report the new coverage wording (*interface-level review*) in `presentCoverage`; bump the
   Fly image's Lain pin and re-run the README memory table; consider whether PR-on-request can be switched on (that is
   the user's call, not the implementer's).

## Equivalence suite (the acceptance test)

For each PR: NVIDIA/OpenShell #5052, NVIDIA/NemoClaw #13812, spuentesp/openshell #1 and #2,
NVIDIA/OpenShell #4995, NVIDIA/OpenShell #6003. Run full mode and layered mode on the same commits and require the
same `diff_contracts.changes`, the same `trace_impact` paths for each traced endpoint, the same `Binds` set, and
coverage that is equal or *more* explicit. Any difference is a bug until explained. A layered result with fewer
findings and no matching gap is the failure this whole project must not produce.

Also: a multi-repo PR (both ends differ in two repos), a consumer→provider→consumer chain (A calls B, B's change
reaches C), a removed endpoint (traced on base, as Parallax does today), and a repo with only unresolved client calls.

## Targets (measure with the Parallax podman recipe in `deploy/fly/README.md`)

- One single-repo PR analysis: peak at most live graph + 2 × changed repo + interface summaries + drilled consumers.
  For #4995 that should be well under 4 GB total (today 5.6 GB).
- At rest after N analyses: back to start-up memory ± a small margin (today +2.5 GB after two).
- No change in wall time worse than +20% for the demo PRs.

## Rules for whoever picks this up

- Lain: work in a **new worktree** of `/data/agents/orca/lain` (`git worktree add`), never check out in the existing
  one (it is on `feat/nat-http-contract`, owned by another session).
- Parallax: follow `AGENTS.md` (no `vi.mock` of internal modules; state under test comes from the real write path;
  every claimed stage observable in a test).
- Do not enable `PARALLAX_REQUEST_ALLOWLIST` on the Fly demo machine.
- Ask the user before pushing, opening PRs, or changing the Fly deployment.

## Open questions to settle in phase 0

1. Which `GraphBackend` methods do snapshot tools really call? That decides composite trait vs. narrowed trait.
2. Does anything in `trace_impact` / `get_service` need nodes from a repo it did not reach through a bind
   (e.g. name search across the federation)? Each such call is a place layered mode must drill or declare a gap.
3. Is the live federation's per-repo projection stable enough to share layers with snapshots later?
4. How much of the 5.6 GB is the hydrate copy vs. the projection vs. the residents? The harness answers this and tells
   which phase pays most.

---

## Phase 0 findings

### Q1. Which `GraphBackend` methods do the snapshot tools call?

`resolve_view` (`mcp/contract_tools/view.rs`) hands every contract tool the snapshot as
`Arc<dyn GraphBackend>`. Calls under `mcp/contract_tools/` (tests excluded):

| Method | Calls | Where |
|---|---|---|
| `get_node` | 19 | everywhere (point lookups) |
| `find_nodes_by_name` | 4 | `analysis.rs` (`caller_node_id`, `enclosing_sender`: filtered to one repo), `trace_flow.rs` |
| `edges_to` / `edges_from` | 4 / 3 | `analysis.rs`, `trace_flow.rs` |
| `traverse_impact` | 2 | `analysis.rs` (`trace_impact`, consumer/provider paths in `diff_contracts`) |
| `list_nodes` | 2 | `evidence.rs` (`same_symbol_at_other_line`, `innermost_node_at_line`), through the concrete `fed.backend` |
| `all_edges` | 1 | `trace_flow.rs::build_adjacency` |
| writes (`upsert_*`) | 4 | test-only fixtures in `contracts.rs`/`context.rs` |

Not called: `query_graph_view*`, `visit_*`, `traverse`, `find_path`, `subgraph_around`, `remove_*`.

Outside the trait: the **raw snapshot tools** (`handler.rs` ~L862: `query_graph`, `find_symbol`, `get_context`,
`get_call_sites`, `get_cross_runtime_callers`, `explain_symbol`, `assess_change`, `explain_dispatch` with a
`snapshot` argument) take `snapshot_federation.backend.db()`, the concrete merged `GraphDatabase`, and run the
ordinary single-graph tools over it (scoped by `set_symbol_repo_scope` when `repo_id` is given).

**Decision: (a), implement `GraphBackend` for a `LayeredBackend`.** The tool surface already goes through
`dyn GraphBackend`; a narrowed trait would touch every tool for no gain. Writes go to the overlay (snapshot
writes are only the post-projection `Binds` + Rust RPC facts). The concrete-type escapes need a change of
their own: the three `fed.backend.list_nodes()` / `.as_ref()` uses in `evidence.rs`/`analysis.rs` move to the
trait (and to a per-repo scan, since they filter to one repo), and the raw tools get the *layer's*
`GraphDatabase` when `repo_id` names one repo. Without `repo_id`, they need either a merged view or a
declared gap (decide in phase 1; Parallax's `stepCodeImpact` does not use snapshot raw tools today).

### Q2. Does anything need nodes from a repo the trace did not reach through a bind?

- `traverse_impact` (`PetgraphBackend`, `graph_backend.rs` ~L590) builds an id→index map and an adjacency over
  **every edge of the federation** per call, then runs a BFS. The walk crosses repos only through edges whose
  endpoints are in two repos. **Measured** (`mem_probe_edges`, all four snapshots of #4995 and #6003): the only
  cross-repo edges are the join's `Binds` (47 of ~310k). Projection writes none: a per-repo layer is
  self-contained, and the overlay owns every cross-repo edge. In layered mode, reaching a bind into repo R is
  what loads R's layer.
- `find_nodes_by_name` in `analysis.rs` is always filtered to the caller's repo afterwards: a one-layer lookup.
- `evidence.rs` `list_nodes` scans are filtered to one repo: one-layer lookups.
- `trace_flow.rs` (`trace_flow` tool) builds adjacency over `all_edges()` and name-searches across the whole
  federation: a whole-federation tool. In layered mode it must load every layer or declare a gap. Parallax does
  not call it on snapshots.
- The raw snapshot tools without `repo_id` do a cross-repo name search (`ambiguous_symbol` check): same as above.

### Q3. Can live federation layers be shared with snapshots later?

Not as things stand. Live repos are indexed in `IndexMode::Live` (LSP pass, cross-repo resolver, overlay,
co-change, NLP), and snapshot layers in `IndexMode::Snapshot`, which skips all of those. So the two graphs
differ even at the same commit. Live is also mutated in place (stale retraction, anchor-miner nodes,
`CrossRepoSameSymbol` reconciliation, re-projection on reload). Sharing would need live to *consume* a snapshot
layer plus a live-only delta, which is a bigger redesign. **Out of scope for this plan**; snapshot↔snapshot only.

### Q4. Where does the memory go? (baseline)

**Basis.** Engine A only (OpenShell, NemoClaw, NeMo-Fabric at the Fly pins), one local `lain server` process
with the Fly image's settings (no language servers, `LAIN_DISABLE_COMMIT_SYNC=1`,
`LAIN_DISABLE_FILE_WATCHER=1`), mimalloc, inside a `systemd-run` scope with `MemorySwapMax=0`. Footprint is
RSS + swap, sampled every 200 ms by `scripts/measure_pr_memory.py`; per-phase numbers come from `LAIN_MEM_PROBE=1`.
The live graph is restored from its checkpoint (warm). "Cold PR commits" means the index-cache entries of the
four PR commits were deleted, and nothing else: exactly four `index_job`s ran, which is the situation on Fly
for a new pull request. This is **not** the Fly container figure, which also includes engine B and the API.

| MB, two PRs back to back (#4995 then #6003) | untrimmed, warm | untrimmed, cold PR commits | **trimmed, cold PR commits** | trimmed, warm |
|---|---|---|---|---|
| engine when it first answers (`ready_threshold: 0`, startup still finishing) | 1 246 | 1 246 | 1 241 | 1 246 |
| settled after a restored start, process tree incl. git sidecars (separate runs) | 1 334 | 1 334 | **655** | 655 |
| #4995 peak | 2 939 | 3 866 | **2 522** | 2 080 |
| #6003 peak | 3 475 | 5 757 | **2 889** | 2 496 |
| at rest after both (2 snapshots still resident) | 2 551 | 4 377 | **1 435** | 1 423 |
| wall: #4995 base / head / diff (s) | – | 27.1 / 26.0 / 19.8 | 26.6 / 25.7 / 19.6 | – |
| wall: #6003 base / head / diff (s) | – | 31.6 / 27.9 / 27.3 | 31.1 / 28.0 / 26.2 | – |

Runs: A, P3, P2 and P1 in `target/bench-fixture/` (not committed). The settled row comes from separate
starts with `LAIN_GIT_SIDECAR_BIN` set as on Fly: the startup trim fires when the checkpoint restore completes
and frees ~690 MB in 35 ms. "Trimmed" means the `heap::trim` fix in this
branch. The untrimmed cold run reproduces the Fly numbers for one engine: 3.9 → 5.8 GB peaks, and +3.1 GB at rest.

**The Fly container itself** (the README's podman recipe: the deployed image, 4 GB cap with swap allowed,
`memory.current + memory.swap.current` of the whole container: both engines, the API, the git sidecars).
"0.16.2" is the deployed image; "this branch" is the same image with this branch's `lain` and
`lain-git-sidecar` (built on Ubuntu 24.04) and re-baked with `bake.sh`. Both have the #4995/#6003 commits
fetched as branches, outside the baked snapshot cache; engine A driven as above.

| MB, whole container | 0.16.2 | this branch |
|---|---|---|
| settled 90 s after engine A is ready | 2 095 | 1 757 |
| #4995 peak | 5 324 | **4 262** |
| #6003 peak | 6 213 | **4 329** |
| at rest after both | 4 779 | **2 784** |
| wall: #4995 base / head / diff (s) | 141.6 / 33.0 / 24.7 | 128.9 / 31.5 / 25.2 |
| wall: #6003 base / head / diff (s) | 41.3 / 41.6 / 36.3 | 34.9 / 29.2 / 28.7 |

The 0.16.2 column reproduces the README's own figures (peaks 5.6 / 6.2 GB, rest 5.1 GB). The container
target of this plan ("well under 4 GB" for #4995) is **not** met: the remaining peak is inside the index jobs
themselves (the trim runs after each job), plus engine B and the API (~1.2 GB). On the 7.9 GB Fly machine the
headroom at the worst peak goes from ~1.7 GB to ~3.6 GB.

**What a phase costs** (live data, read after `mi_collect` + `malloc_trim`, runs B/F/H):

| Phase | Live after | Transient peak | Notes |
|---|---|---|---|
| one resident snapshot federation (3 repos, ~100k nodes, ~310k edges) | **~0.29 GB** | – | dropped on eviction (verified: `federation_dropped`) |
| hydrate + project, per repo | – | +0.1 to +0.4 GB | NemoClaw is the largest |
| `rust_rpc::resolve` | ~0 | +0.5 to +0.7 GB | reads and parses every `.rs` blob of the snapshot through libgit2/tree-sitter |
| join | ~0 | ~+0.2 GB | |
| `trace_impact`, `get_service`, `get_coverage` | ~0 | < 20 MB | |
| snapshot index job (one OpenShell commit) | ~0 | **0.8 to 1.2 GB** freed by the trim after it | |

**The finding that changes the plan.** Most of the growth was not snapshot data. It was **freed memory that
glibc kept**. Lain's Rust heap is mimalloc, which returns pages by itself. libgit2, tree-sitter, zlib and the C++
runtime allocate through glibc `malloc`, and glibc keeps their frees in per-thread arenas:
- about 0.9 GB right after startup (the engine goes from 1.47 to 0.55 GB on one `malloc_trim(0)`);
- about 0.7 GB after the first snapshot build;
- 0.8 to 1.2 GB after every index job.

`mi_collect` cannot reach any of it. `RssAnon` stayed at ~0.7 GB over start with every federation dropped, until
`malloc_trim` was added. The static `PreparedRepository` LRU and the parse cache were suspected and ruled out (the
first is empty in snapshot builds; the second is off by default).

Fixed in this branch (`src/server/heap.rs`): trim after startup indexing/restore, after each snapshot build,
after each index job, plus a rate-limited idle trim. Each trim takes 8 to 44 ms, and wall times are unchanged
(table above). The shipped binary is `x86_64-unknown-linux-gnu` and the Fly image runs glibc 2.39, so the fix
applies there.

**Residency.** `LAIN_SNAPSHOT_RESIDENT_IDLE_SECS=120` does bring memory back once trimming is on (0.83 GB at
rest with both residents dropped, run H). Parallax's EXPLAINING step and the UI use the head snapshot after the
analysis, so dropping it costs a rebuild later (~10 s with a warm cache). With trimming, keeping two residents
costs ~0.6 GB. Left at the current default (off); this is the user's call.

Also seen: a new federation is built *before* the LRU entry it replaces is evicted (`install_resident_held`),
so a build at the cap briefly holds cap + 1 federations: ~0.3 GB extra, now that a federation is 0.29 GB.

### What this means for phases 1–3

The plan assumed the duplicated per-repo projections were the problem. Measured, a whole resident snapshot is
~0.29 GB, so Part 1 (layer sharing) saves at most the unchanged repos' share of that per snapshot: about
0.2 GB for a single-repo PR. Part 2 (interface-first) would mainly cut the build *transients*: hydrate/project
and `rust_rpc`'s read of every Rust blob.

With the trim, for these two PRs:
- engine A peaks at 2.5 GB; the whole container at 4.3 GB. That is short of "well under 4 GB" for the
  container, by about the index jobs' own transient (see the container table);
- at rest, memory comes back to start + resident snapshots (± ~0.2 GB) on engine A, and the container rests
  2.0 GB lower than 0.16.2;
- wall time is unchanged or better.

Not yet verified: the other demo PRs.

Cheaper next steps, in order of measured payoff:
1. The index job's own transient (0.8–1.2 GB of C-heap churn per OpenShell commit, freed only when the job
   ends). Trimming inside the job, between indexing phases, or bounding glibc's arenas
   (`M_ARENA_MAX`), would cut the container peak further.
2. `rust_rpc`: open each mirror once per build (today `open_bare` runs per file read), and cache its per-repo
   wrapper/candidate scan per (repo, commit) like `graph.bin`. Cuts the +0.5–0.7 GB transient and part of the
   build time.
3. Evict before building when at the cap (saves one federation, ~0.3 GB, at peak).
4. Only then reconsider phases 1–3; the equivalence suite remains their gate.
