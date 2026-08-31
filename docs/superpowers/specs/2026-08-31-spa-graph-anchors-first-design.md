# SPA Graph tab: anchor-first default + on-demand neighbourhood

Date: 2026-08-31
Status: design (supersedes the v1 spec's "render-the-full-graph" default)
Owner: SPA-only; no server changes
Follows from: the prior v1 SPA-graph-upgrade spec at `docs/superpowers/specs/2026-08-31-spa-graph-upgrade-design.md`

## Problem

The v1 SPA upgrade landed a graph tab that calls `get_workspace_graph` at default. At federation scale (5 000 indexed nodes for `bytes + tokio`) the result is a hairball:

- **Lines become invisible.** At 5 000 nodes × 3 700 edges the canvas is dominated by overlapping node fills; edges are obscured. The user reported "I see no lines."
- **Architecture meaning is lost.** The default view is symmetric, not hierarchical — every node looks equally important, so the tab can't answer "what holds this codebase together?"
- **Performance is awful.** 5 000 SVG `<path>` nodes + 3 700 `<line>` elements with hover handlers and a force simulation running every tick is sufficient to make the tab feel sluggish in Chromium.
- **The frame we recorded for the hero** confirms this — teal and amber dots scattered with no readable structure.

The user-feedback that triggered this redesign: *"the spa doesn't show the graph relationships… I'd limit and use anchors unless a specific symbol or file is requested — and we need the lines. I see no lines."*

## Goal

The default Graph tab is a small, readable graph: ~50-100 nodes, ~80 edges, lines visible, architecture meaning preserved. The user can focalize any symbol's neighbourhood on demand.

The user gets the "what holds it together" answer at first sight, and the "what does this symbol depend on / who calls it" answer via click or search.

## Approach

The default render is anchored on **`find_anchors`** (existing MCP tool — returns ranked symbols by centrality). The full graph (`get_workspace_graph`) is no longer the default payload; it remains the fallback for the focalised view's neighbourhood expansion when needed but is not what the user sees first.

Three render modes under one tab:

1. **Default — anchor view.** Top 30 anchors from `find_anchors` + their 1-hop neighbourhood (union-deduped against the workspace graph). Rendered with the existing per-repo colour, per-kind shape, hover focus, minimap, and legend.
2. **Focalised — single-symbol neighbourhood.** Click any node (in any view) → fetch `get_blast_radius { symbol, depth: 1 }` (default) for that one symbol → canvas shows just that node's hub-and-spoke. Use the depth slider (1/2/3) to widen. The anchor view's filter chips still apply.
3. **Search-driven focalisation.** Type a symbol name in the search input → resolves via `explore_architecture` (or `find_anchors` with a like-filter) → focalises on it as if clicked.

All three modes share the same colour/shape/zoom/minimap/hover-focus/legend rendering layer from the v1 spec. The filter-bar's repo + kind chips apply to whichever mode is active.

**Visible-lines guarantee**: at the default 50-100 node density, edges are clearly readable; cross-repo lines in `var(--warn)` orange stand out; no text overlap because the per-kind text only renders at this scale.

## Components

| File | Responsibility |
|---|---|
| `src/server/mcp/command_center/app.js` | Rewrite `drawGraphSvg` (and the accompanying helpers) to operate over a "visible set" derived from the active mode's payload rather than the full workspace graph. Add mode state (`anchor | focal`), search input wiring, depth slider wiring, "back to anchors" button. Keep all existing helpers (`computeRepoPalette`, `applyFilters`, `nodeShape`, `wireZoom`, `paintMinimap`, `paintLegend`, `onHover`). |
| `src/server/mcp/command_center/index.html` | Add a search input + a depth slider + a "back to anchors" button to the existing `.graph-filter-bar` (Task 4 already laid the bar down). Keep the data-filter-bar slot; the new controls live alongside the existing filter chips. |
| `src/server/mcp/command_center/styles.css` | Add minimal rules for the search input + slider + button (Task 4's existing chip rules carry over). ~20 lines. |
| `tests/js/graph_tab.test.js` | Add 2-3 pure-helper tests for the new visible-set reducer: `computeAnchorVisibleSet(anchors, workspaceGraph)` and `applyDepth(neighbourhood, anchor, depth)`. Pure, DOM-free, exported via the existing CommonJS footer. |
| `tests/js/spa_e2e.test.js` | Add 4 new Graph-tab assertions: anchor view's `#graph-canvas path.graph-node` count is ≤ 150 (post-federation-load); clicking an anchor node replaces the visible set (count changes); the search input focalises on typed name; "back to anchors" restores the original visible set. |

Untouched (explicitly):

- `src/server/**` (no Rust changes; `find_anchors`, `get_blast_radius`, `get_workspace_graph`, `explore_architecture` all exist).
- `scripts/**`, `.github/workflows/**`, `Cargo.*`, README, `docs/**` files outside `src/server/mcp/command_center/**`.

## Data flow

```
User opens Graph tab (or clicks an existing filter chip → reset to anchor mode)
    ↓
renderGraphTab()
    ↓
loadGraphWorkspaces() — pick the active workspace
    ↓
fetch find_anchors { repo_id?: <selected repo>, limit: 30 }
    ↓
fetch get_workspace_graph {} — ONLY for neighbourhood lookup
    ↓
computeAnchorVisibleSet(anchors, workspaceGraph)
    - Each anchor's 1-hop neighbourhood: nodes that appear as either source or target
      in any edge incident to the anchor (via workspaceGraph.edges)
    - Union-dedup the anchor set + the per-anchor neighbourhoods
    - Filter edges to endpoints both in the union
    ↓
drawGraphSvg(svg, { nodes: visibleNodes, edges: visibleEdges, truncated: false })
    ↓
paintLegend / paintMinimap / wireZoom / onHover / buildFilterBar
    ↓
User clicks a node
    ↓
fetch get_blast_radius { symbol: <node.name>, depth: <slider value> }
    ↓
drawGraphSvg(svg, { nodes: br.nodes, edges: br.edges, truncated: br.truncated, mode: 'focal' })
    ↓
User clicks "back to anchors" → re-run anchor-mode fetch + render
```

The full workspace graph is fetched once (cached in closure state) and reused for any subsequent anchor-set computation or fallback expansion. The pagination-doesn't-fire-after-default-fetch concern: with 5 000 nodes × 3 700 edges ≈ 1 MB JSON, one fetch per workspace switch is acceptable.

## State

Closure-local `renderGraphTab()` state:

```js
const state = {
  mode: 'anchor',                          // 'anchor' | 'focal'
  anchors: [],                             // top-N find_anchors result
  workspaceGraph: null,                    // cached get_workspace_graph payload (raw)
  visibleNodes: [],                        // computed from mode + active workspace
  visibleEdges: [],
  depth: 1,                                // focal view's blast_radius depth; slider 1..3
  searchQuery: '',
};
```

Plus the existing filter-state — repos, kinds, crossRepoOnly, labelsAlwaysOn — from the v1 spec; unchanged.

## Edge selection in anchor mode

For each anchor in the anchor set, the visible-set selector includes its 1-hop neighbourhood (any node that appears as source OR target in any edge incident to that anchor). Edges visible are those whose both endpoints are in the union.

This means:

- An anchor with degree 50 brings 50 neighbours in.
- An anchor with degree 5 brings 5 in.
- The visible edges include every anchor-to-anchor edge that exists in the workspace graph (they all qualify), plus every anchor-to-non-anchor edge where both endpoints are visible.

Anchor list from `find_anchors` is sorted by centrality (descending). The first 30 are taken. The visible set is presented to D3 in this order so the most-central anchors land visually first.

## Edge styling (carries over from v1)

- `var(--warn)` stroke for cross-repo edges (`e.cross_repo === true`).
- `var(--border-strong)` for intra-repo edges (already covered at lower opacity).

`stroke-width` bumps from 1.5 → 2 for cross-repo only at this density (was 1.5 in v1). The 1.5 looked good at 5k nodes; at 50-100 nodes 2 reads even better. Trivial change.

`stroke-opacity` for intra-repo: bumps from 0.6 → 0.75 — at low density we can afford to be more legible.

Both changes are inside `styles.css` `.graph-link` / `.graph-link.cross-repo` rules. One commit.

## UX details

- **Filter chip behaviour in anchor mode**: clicking a repo chip removes anchors in the unselected repo from the visible set; clicking a kind chip removes non-matching-kind nodes. The selector stays anchor-shaped (top-N find_anchors) but the visible set is the intersection. Toggle "cross-repo only" restricts edges to `e.cross_repo === true`. Toggle "labels always" forces text rendering even if the visible set is small (always safe at 50-100 nodes; the v1 150-node threshold is moot here).
- **Filter chip behaviour in focal mode**: chips apply to the focal view's nodes/edges directly. Anchor view becomes inaccessible from chips; user clicks "back to anchors" to return.
- **Search input**: `<input type="search" placeholder="focus on symbol…">` in the filter bar. Live search: on `input` event with debounce ~300 ms, fire `tools/call find_anchors { repo_id?: null, limit: 10 }` then look up whether the typed name matches any anchor's name (case-insensitive substring); if exactly one match, focalise on it (via `get_blast_radius { symbol }`); if multiple matches, show a small inline list to pick from.

  Empty input or no match → no-op (the search doesn't break the anchor view).

- **Depth slider**: `<input type="range" min="1" max="3" value="1">` in focal mode only (hidden when mode='anchor'). Tied to the active focal node — bumping it re-fetches `get_blast_radius { depth }` and re-renders. Range 1..3 matches the tool's accepted depth format.

- **"Back to anchors" button**: in focal mode only; restores the anchor-mode fetch + render.

## Error handling

- `find_anchors` returns empty (no anchors in the workspace) → fall back to a one-line empty-state hint: *"Workspace has no anchors. Pick a smaller repo or type a symbol to focalise."* — same pattern as the existing `renderGraphTabEmpty` but for the anchor-vs-fullwork split.
- `get_blast_radius` returns errors (e.g., symbol not found) → keep the previous visible set on screen, surface a transient stderr message.
- Search input returns no matches → leave the anchor view active; no-op.
- Workspace-graph fetch fails → degrade to anchor-only (no 1-hop expansion); the visible set is just the top-30 anchors (very small, ~30 nodes max).

## Testing

### Pure-helper tests (Task 2's prior slot, in `tests/js/graph_tab.test.js`)

```js
function computeAnchorVisibleSet(anchors, workspaceGraph, opts = {}) {
  // Pure: returns { nodes, edges, hiddenNodeIds }.
  // Default opts.neighbourhoodDepth = 1 (anchors' 1-hop neighbourhood).
  // For each anchor in `anchors`, find its incident edges in `workspaceGraph.edges`;
  // include the anchor + every neighbour. Union-dedup, then filter edges to
  // endpoints both in the union.
}

function applyDepth(neighbourhood, anchor, depth) {
  // Pure: returns the visible-set for a focal node at `depth`.
  // Currently just returns `neighbourhood`. Will be expanded if the
  // implementation needs more from it.
}
```

Tests cover:

- `computeAnchorVisibleSet` with a 5-anchor / 30-edge fixture: returns a visible set with all 5 anchors plus their neighbours, edges filtered to endpoints in the visible set.
- `computeAnchorVisibleSet` with two anchors that share a neighbour: that neighbour appears once.
- `computeAnchorVisibleSet` with a workspace graph that has no edges incident to an anchor: the anchor itself still appears, but no neighbour/edge additions.

### SPA e2e tests (Task 6's prior slot, in `tests/js/spa_e2e.test.js`)

- After the Graph tab loads in anchor mode, `path.graph-node` count ≤ 150 (post-test-fixture-load).
- Click an anchor node in the visible set → after the click, `get_blast_radius` was called (verify via `chrome devtools` polling or a probe) AND the rendered `path.graph-node` count changes (it becomes the focal neighbourhood, not the anchor set).
- Type a known symbol name into the search input → after debounce, the focal mode is active; canvas re-renders with that symbol's neighbourhood.
- Click "back to anchors" → the visible set reverts to the original anchor set.

### Recorder driver (`tests/js/record_spa_demo.js`)

The Graph-tab settle `setTimeout` reverts from 10 s → 6 s. The anchor view has ≤100 nodes, so the force layout settles faster than the prior 5 000-node hairball.

### Hero recording

Re-record with the new graph tab. The README hero will replace the prior 5000-node hairball with a ~50-node anchor view — visually communicative, lines readable.

## Verification gates

1. `cargo test --test cli_surface` passes (commands table untouched — no Rust changes).
2. `node --test tests/js/graph_tab.test.js` passes (29+ tests including new pure helpers).
3. `node tests/js/spa_e2e.test.js` passes (47+ tests including 4 new Graph assertions).
4. `bash scripts/smoke_federation_fixture.sh` passes (no fixture changes).
5. `make record-demo` produces the upgraded hero artifacts; frame at t≈25 s shows the anchor view (small readable graph with visible edges).
6. Working tree clean apart from pre-existing untracked files.

## Risk register

| Risk | Mitigation |
|---|---|
| `find_anchors` returns a workspace-wide default; user might want per-repo anchors | Filter bar already has repo chips. In anchor mode, the top-N is filtered by the currently-selected repos before the visible-set computation. If no repo is selected → empty hint. |
| Cross-repo edges (`var(--warn)`) still 0 for the bytes+tokio pair (per the prior frame's "0 cross-repo" metadata) | The cross-repo line styling is unaffected; the federation-resolution 0-count is pre-existing and parked, not a Task concern. Anchor view's value stands even at 0 cross-repo lines because intra-repo lines become readable. |
| Top-30 may exclude the most-relevant edge for a focal target | "Back to anchors" button + search input cover the on-demand cases. If 30 anchors miss something the user wants, the focal view surfaces it. |
| Search debounce renders the UI as "ready" before the focal results land | Acceptable — the previous visible set stays on screen until the new one renders. The user perceives "graph updates when I type" rather than "graph flickers as I type". |
| 1-hop expansion may overshoot for low-degree anchors (5 in-neighbours each → manageable); or undershoot for the very-high-degree anchor case (e.g., tokio's runtime hub with 100+ in-degree → we don't pull all 100 into the visible set; we cap to top-30 by neighbourhood centrality or just stay with whatever came back). | Cap per-anchor neighbourhood contribution at 20 nodes for any single anchor. Choose the 20 most-central neighbours first (the workspace graph's repo-ids hint central nodes by their own degree). The cap is configurable via a slider later if needed; v1 ships with the default 20. |
| Recorder still rebuilds needed | `app.js` is `include_bytes!`'d into the Rust binary. `cargo build --release` is required before re-recording. Documented in the SDD ledger. |

## Files touched

Modified:

- `src/server/mcp/command_center/app.js` — rewrite `drawGraphSvg` for the anchor-first data flow; add `computeAnchorVisibleSet`, `applyDepth`; extend filter bar wiring for the search input + slider + back-to-anchors button.
- `src/server/mcp/command_center/index.html` — add `<input type="search">`, `<input type="range">`, `<button>` elements inside `.graph-filter-bar`.
- `src/server/mcp/command_center/styles.css` — search + slider + button rules (~20 lines); bump `.graph-link.cross-repo` stroke-width to 2; bump `.graph-link` stroke-opacity to 0.75.
- `tests/js/graph_tab.test.js` — 3 new pure-helper tests.
- `tests/js/spa_e2e.test.js` — 4 new Graph-tab assertions.
- `tests/js/record_spa_demo.js` — Graph hold 10 s → 6 s.
- `docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}` — re-recorded hero artifacts (anchor view at t≈25).

Untouched (explicitly):

- `src/server/**` (no Rust changes; `find_anchors` and `get_blast_radius` and `get_workspace_graph` are existing tools).
- `scripts/**`, `.github/workflows/**`, `Cargo.*`, README, `docs/**` files outside `src/server/mcp/command_center/**`.

## Out of scope

- Server-side anchor graph query (e.g. `get_anchor_graph { limit, depth }`). Two MCP round-trips (`find_anchors` + `get_workspace_graph`) on the client are tractable; if profiling later shows this dominates load time, we'll add a server-side tool. Not in this PR.
- Per-file focalisation (instead of per-symbol). `get_blast_radius` is symbol-keyed; file-keyed neighbourhood would need a different tool or a `find_anchors` variant by file. Skip per scope.
- Saving the user's last view in `localStorage` (so the tab reopens at the focal they left). YAGNI for v1.

## Spec self-review

- No TBD / TODO / placeholders.
- Internal consistency: all three modes share the colour/shape rendering layer; v1 spec's filter chips apply in both modes; the "back to anchors" button is mode-conditional.
- Scope: single implementation plan (one workspace, three render modes, ~3-5 files).
- Ambiguity: searched for "may", "could", "either-or" wording on the spec text — none that need disambiguation. The "depth 1..3" matches the existing MCP tool's accepted format. The "anchors 30" default is a constant; explicit in the data flow and state sections.
