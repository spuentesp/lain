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
