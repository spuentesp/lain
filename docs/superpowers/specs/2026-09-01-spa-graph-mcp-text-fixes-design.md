# SPA Graph tab: live-MCP path fixes (Items 13 + 14)

Date: 2026-09-01
Status: design
Owner: SPA-only; no server changes
Follows from: the v2 SPA Graph-upgrade plan at `docs/superpowers/plans/2026-08-31-spa-graph-anchors-first.md`; this design addresses the live-MCP-path bugs surfaced by the final-review fix wave.

## Problem

The v2 SPA Graph tab has two live-MCP-path bugs that prevent the user's "lines visible" goal from being realised in the live SPA (and the recording):

- **Item 14 — `find_anchors` requires `repo_id`.** The handler at `src/server/mcp/handler.rs:88-90` confirms `find_anchors` is in the `requires_repo_scope` set (only `query_graph` is federation-wide). The recorder driver at `tests/js/record_spa_demo.js:278` already passes `repo_id: 'bytes'` explicitly — that's why the recorder works. The SPA's `renderGraphTab` anchor branch at `app.js:1610` calls `mcpCall('find_anchors', { limit: 30 })` without `repo_id`, so the federation's scope guard rejects the call and the parser sees empty text. Effect: anchor mode shows "no anchors" instead of an anchor graph.
- **Item 13 — `get_blast_radius` returns text, not JSON.** The handler at `src/server/tools/handlers/impact.rs:55-259` builds a multi-section text report (`**Bytes**\nDirect:\n  - caller path\n  - ...`); the schema at `src/server/mcp/definitions.rs` declares `symbol` and `include_coupling` only — **no `depth` parameter** (the v2 spec's depth slider is silently ignored). The SPA focal branch at `app.js:1589` runs the result through `parseJson`, which always returns `null` for this text format; `normalizeGraphPayload(null)` produces an empty graph. Effect: focal mode shows "No graph data for <symbol>" instead of the user's neighbourhood.

Both bugs share a single cause: the SPA's data-fetch path was written against the wrong tool-shape assumptions. The MCP surface is two-tier:

- Per-repository tools (`find_anchors`, `get_blast_radius`) — text format. The recorder driver handles these via regex.
- Federation-scoped tools (`get_cross_repo_blast_radius`, `_for_repo`) — JSON. The SPA was written against these shapes.

## Goal

Both modes (anchor + focal) render their visible sets with real nodes and edges when the SPA boots against `bytes + tokio`. The existing v2 design and recorder pipeline carry through unchanged; the only deltas are:

1. Anchor branch gets the active workspace's first-repo `repo_id` and calls `find_anchors` with that param.
2. Focal branch switches from `get_blast_radius` (text, no depth) to `get_cross_repo_blast_radius_for_repo` (JSON, accepts `depth: "1..3"`).

The recording frame captured at t≈35 s after re-record shows the focal graph with visible nodes/edges.

## Approach

### Item 14 — `find_anchors` accepts `repo_id`

The SPA's active workspace graph is a normalised payload with `nodes[*].repo_id` populated. The simplest robust wiring: pick the **first distinct `repo_id`** from `workspaceGraph.nodes` (which the existing closing ceremony after `get_workspace_graph` returns) and pass it.

Concretely, in `renderGraphTab`'s anchor branch (around `app.js:1602-1659`), change:

```js
anchorsResult = await mcpCall('find_anchors', { limit: 30 });
```

to:

```js
// Item 14: anchor view is per-repository by federation design. Pick the
// first distinct repo_id from the workspace graph.
const anchorRepoId = (() => {
  for (const n of workspaceGraph.nodes) {
    if (n.repo_id) return n.repo_id;
  }
  return null;
})();
anchorsResult = await mcpCall('find_anchors', {
  repo_id: anchorRepoId,
  limit: 30,
});
```

The text-format parser from Task 4 round 2 (commit `ae78f7f`) — `/^\s*\d+\.\s+([A-Za-z_][A-Za-z0-9_]*)/gm` — is **unchanged**; the format is identical. The empty-state hint fires only if `anchorRepoId` is null AND no anchor lines parsed. Both branches of the failure are caught in the existing fallback at `app.js:1642-1645`.

Tradeoffs:

- The anchor view is single-repo rather than federation-wide. The spec's "anchor set + 1-hop neighbourhood" framing holds: with 30 anchors from `bytes` (or whichever repo has the most), the user gets a clear architecture view.
- We could in principle iterate over each repo in the workspace and merge, but the realistic gain is small: at federation scale, 30 anchors from one repo already cover the dominant architecture. YAGNI.

### Item 13 — focal mode uses `get_cross_repo_blast_radius_for_repo`

Switching tools has compounding benefits over a text-format parser:

- **It accepts `depth`**, so the v2 spec's depth slider actually does something. (The current `get_blast_radius` schema doesn't accept depth, so the slider is silently ignored — a spec-vs-impl mismatch.)
- **It returns JSON** with a `by_repo: BTreeMap<String, Vec<String>>` shape, so we can parse directly with `JSON.parse`. No regex parsing of multi-section text.
- **It also works across repos**, mirroring what the v2 spec called for in the focal-but-cross-repo UX.

Concretely, in `renderGraphTab`'s focal branch (around `app.js:1557-1600`), change:

```js
result = await mcpCall('get_blast_radius', { symbol: focalSymbol, depth: String(focalDepth) });
```

to:

```js
result = await mcpCall('get_cross_repo_blast_radius_for_repo', {
  repo_id: focalRepoId,        // from the clicked node (`d.repo_id`)
  symbol: focalSymbol,
  depth: `${focalDepth}..${focalDepth}`,   // "1..1", "2..2", "3..3" — schema accepts a range
});
```

The shape change ripples through two helpers in the same `renderGraphTab`:

1. `parseJson(result)` works directly (no fallback to text parser).
2. The visible-set construction changes from the `get_blast_radius` response (which had `nodes`/`edges`/`truncated` at top-level) to `by_repo`-per-repo plus the focal symbol as the centre. Build the visible set via `computeAnchorVisibleSet`-style union: the focal symbol is the centre, every symbol listed in `result.by_repo[*]` is a leaf (and `cross_repo` between the focal repo and each leaf repo is detected from the per-symbol bucket).

Both deltas are inside `renderGraphTab`'s data-fetch branch and don't touch `drawGraphSvg`'s render helpers.

### Where the focal repo id comes from

The focal branch already tracks `graphState.focalSymbol`. We also need `graphState.focalRepoId` — set when the user clicks an anchor node in `drawGraphSvg`'s click handler:

```js
node.on('click', (event, d) => {
  graphState.focalSymbol = d.name;
  graphState.focalRepoId  = d.repo_id;          // <-- new
  graphState.mode        = 'focal';
  renderGraphTab();
});
```

The search-input path needs the same fix: when the user types a symbol, lookup `repo_id` via the workspace graph (`workspaceGraph.nodes.find(n => n.name === q)?.repo_id`) and pass that as `graphState.focalRepoId`. If the typed name exists in multiple repos, disambiguate by the workspace's first-repo convention (same as the anchor branch).

## Components

| File | Responsibility |
|---|---|
| `src/server/mcp/command_center/app.js` | Modify the anchor and focal branches inside `renderGraphTab`. Update `drawGraphSvg`'s click handler and the search-input wiring inside `wireGraphControls` to set `graphState.focalRepoId`. Build the visible-set from `get_cross_repo_blast_radius_for_repo`'s `by_repo` shape. |
| `tests/js/spa_e2e.test.js` | Add 1-2 assertions: (a) anchor mode with `path.graph-node > 0` actually renders nodes after the `find_anchors` call carries `repo_id`; (b) focal mode's response contains the requested symbol once rendered. |
| `tests/js/record_spa_demo.js` | None — the recorder driver already uses `find_anchors` correctly (with `repo_id`) in its own code, and the focal-step was already added in the prior fix wave. The recording's `data_mut` symbol from the prior fix wave should now actually render nodes. |
| `docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}` | Re-recorded hero artifacts (the focal frame should now show the visible nodes/edges the user has been asking for). |

Untouched (explicitly):

- `src/server/**` — no Rust changes. The handlers are correct; the SPA was wrong.
- `scripts/**`, `.github/workflows/**`, `Cargo.*`, README, `docs/**` files outside `src/server/mcp/command_center/**`.

## Data flow

```
User opens Graph tab (or clicks an existing filter chip)
    ↓
renderGraphTab()                                                ← unchanged signature
    ↓
loadGraphWorkspaces() — pick the active workspace
    ↓
fetch get_workspace_graph {}
    ↓
mode === 'focal' branch:
    graphState.focalRepoId (set by prior click / search)
    ↓
    fetch get_cross_repo_blast_radius_for_repo {
        repo_id: graphState.focalRepoId,
        symbol: graphState.focalSymbol,
        depth: `${state.depth}..${state.depth}`
    }
    ↓
    parse JSON; build visible set from by_repo + focal symbol as centre
    ↓
    drawGraphSvg(svg, visible)

default anchor branch:
    pick first distinct repo_id from workspaceGraph.nodes[*].repo_id
    ↓
    fetch find_anchors { repo_id, limit: 30 }
    ↓
    parse text-format with regex (Task 4 round 2's parser)
    ↓
    computeAnchorVisibleSet(anchors, workspaceGraph)          ← unchanged from v2
    ↓
    drawGraphSvg(svg, visible)
```

## State

The `graphState` closure object gains one field:

```js
const graphState = {
  mode: 'anchor',          // 'anchor' | 'focal'
  focalSymbol: null,
  focalRepoId: null,       // <-- new in this plan
  depth: 1,
  searchQuery: '',
  workspaceGraph: null,
};
```

Click handler and search-fill handler both write `focalRepoId` alongside `focalSymbol`.

## Error handling

Existing error paths in `renderGraphTab` handle the empty/`isError` cases via `renderGraphTabEmpty`. The new flows reuse those handlers. Specifically:

- `find_anchors` with `repo_id` succeeds but parses to zero anchors → existing empty-state hint ("no anchors") still fires; user can search or click instead.
- `get_cross_repo_blast_radius_for_repo` returns no symbols for the typed query → existing `renderGraphTabEmpty({ mode: 'error', message: "no data for <symbol>" })` fires.

If `focalRepoId` is null when the focal branch runs (defensive — should not happen because click sets it), the call uses the first distinct repo_id from `workspaceGraph.nodes` as a fallback. If even that fails, the existing empty-state hint fires.

## Testing

### Pure-helper tests (extend `tests/js/graph_tab.test.js`)

Add 1 pure helper and 2 tests:

```js
// Builds a visible set from a `get_cross_repo_blast_radius_for_repo` JSON
// payload (focal symbol + by-repo map). Pure.
function applyFocalGraph(payload, opts = {}) {
  // payload shape: { by_repo: { 'repo': [{name, repo_id, kind, path}, ...] }, total_count, truncated }
  // ... returns { nodes, edges, hiddenNodeIds } similar to computeAnchorVisibleSet
}

// Tests:
// 1. applyFocalGraph with a focal symbol listed in by_repo['bytes']:
//    - returned nodes include the focal + all by_repo[*] entries
//    - returned edges are by_repo cross-edges from focal to each by_repo entry
//    - cross_repo flag = true if the symbol's repo_id != the leaf's repo_id
// 2. applyFocalGraph with truncation: truncated=true surfaces in the result (analogous to v2's graph.truncated handling).
```

Plus 2 additional `computeAnchorVisibleSet` tests (already 5; adding 1 more for the explicit-repo case to pin Item 14):

```js
test('computeAnchorVisibleSet: anchor with explicit repo_id matches only that repo, not even when neighbours exist across repos', () => {
  const anchors = [{ name: 'Hub', repo_id: 'r1' }];
  const workspaceGraph = {
    nodes: [
      { id: 'r1-h', name: 'Hub', repo_id: 'r1' },
      { id: 'r2-x', name: 'HubExt', repo_id: 'r2' },
    ],
    edges: [],
  };
  const out = computeAnchorVisibleSet(anchors, workspaceGraph);
  const ids = new Set(out.nodes.map(n => n.id));
  assert.ok(ids.has('r1-h'));
  assert.ok(!ids.has('r2-x'));
});
```

### SPA e2e tests (extend `tests/js/spa_e2e.test.js`)

Add 1 assertion to the existing Graph-tab block:

```js
// Item 13: focal mode renders nodes for the queried symbol.
const v2FocalRendered = await page.evaluate(() => {
  const nodes = document.querySelectorAll('.graph-node');
  const meta = document.getElementById('graph-meta');
  return {
    nodeCount: nodes.length,
    metaText: meta ? meta.textContent : '',
  };
});
assert.ok(v2FocalRendered.nodeCount > 0, 'focal mode rendered nodes for the queried symbol');
```

The existing `v2AnchorNodeCount > 0` assertion (from the prior plan, `976f774`) already pins Item 14's `repo_id` fix once the SPA passes repo_id.

### Recorder

The recorder driver's `data_mut` symbol was chosen in the prior fix wave because it's unambiguous. With Item 13's fix, the focal view will now resolve `data_mut` to a real cross-repo blast-radius graph. No recorder change needed.

## Verification gates

1. `cargo test --test cli_surface` passes (commands table untouched — no Rust changes).
2. `node --test tests/js/graph_tab.test.js` passes (32 prior + 3 new).
3. `node tests/js/spa_e2e.test.js` passes (48 prior + 1 new — total 49).
4. `bash scripts/smoke_federation_fixture.sh` passes (no fixture changes).
5. `make record-demo` produces the upgraded hero artifacts; frame at t≈35 s shows nodes and edges.
6. Working tree clean apart from pre-existing untracked files.

## Risk register

| Risk | Mitigation |
|------|------------|
| `requires_repo_scope` rejects `find_anchors` for a workspace with no `repo_id`-tagged nodes | The `anchorRepoId` selector walks `workspaceGraph.nodes` looking for the first non-null `repo_id`. In the federation-graph payload this is always populated. If null, the existing empty-state hint fires. |
| `get_cross_repo_blast_radius_for_repo` returns no symbols for a search input that doesn't exist | The existing `renderGraphTabEmpty` path handles `isError` and empty results. The user sees the standard "no data for <symbol>" message and types a different symbol. |
| The recorder's `data_mut` symbol resolves against both `bytes` and `tokio` ambiguously | Pre-task implementer checked: `data_mut` has 3 direct + 17 indirect dependents — verified unambiguous. |
| Switching focal mode tools changes visible-graph shape | The v2 spec already documented the focal payload shape (top-level `nodes`/`edges`/`truncated`); `normalizeGraphPayload` accepts the new shape via a slight tweak (applyFocalGraph builds equivalent input to `normalizeGraphPayload`). |
| Repository name in `repo_id: 'bytes'` collides with `path.repo_id` parsing downstream | `repo_id` is just a string. No collision risk. |

## Files touched

Modified:

- `src/server/mcp/command_center/app.js` — anchor branch passes `repo_id`; focal branch switches tools + parses JSON; `graphState.focalRepoId` field; click handler + search-input handler set it; new helper `applyFocalGraph`.
- `tests/js/graph_tab.test.js` — 3 new pure-helper tests.
- `tests/js/spa_e2e.test.js` — 1 new Graph-tab assertion.
- `docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}` — re-recorded hero artifacts.

Untouched (explicitly):

- `src/server/**` (handlers are correct; only the SPA was wrong).
- `scripts/**`, `.github/workflows/**`, `Cargo.*`, README, `docs/**` files outside `src/server/mcp/command_center/**`.
- `tests/js/record_spa_demo.js` (already correct from the prior plan).

## Out of scope

- Iterating `find_anchors` per repo and merging (single-repo is enough; YAGNI).
- Server-side handler changes (the schema's `depth` parameter omission on `get_blast_radius` is parked as a follow-up).
- `applyFocalGraph` adding a server-side depth-filter optimization.
- Disambiguation UI for the search input when a typed name matches symbols across repos (functional in the common case via the first-repo convention).

## Spec self-review

- **No placeholders**: zero TBD/TODO.
- **Internal consistency**: anchor and focal branches both treat `repo_id` consistently (anchor picks it from workspace graph; focal picks it from click/search). The `graphState.focalRepoId` field is the single source of truth.
- **Scope**: single SPA-only change; two logical halves (anchor repo_id + focal tool swap) but they share infrastructure (graphState field + click handler); fits in one plan.
- **Ambiguity**: 
  - "depth:" — explicitly formatted as `${depth}..${depth}` ("1..1", "2..2", "3..3") per the schema at `src/server/mcp/federation_tools/dto.rs:57-62` (fed to `parse_depth`).
  - "anchor repo_id" — explicit policy: first distinct non-null `repo_id` from `workspaceGraph.nodes`.
