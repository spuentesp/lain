# SPA Graph tab: filters, colours, zoom, minimap, hover focus

Date: 2026-08-31
Status: design
Owner: SPA-only; no server changes

## Problem

The Command Center Graph tab (the headline tab of the hero recording) renders the workspace call graph with a plain D3 force layout. At federation scale (5 000 indexed nodes for `tokio-rs/bytes + tokio-rs/tokio`):

- **All nodes are the same colour** — there is no visual distinction between repos, so the dense hairball reads as a single unnamed blob.
- **Labels are suppressed above 150 nodes** (`app.js:902`) — at 5 000-node scale, every circle is anonymous.
- **No zoom or pan** — there's no way to navigate a 5 000-node graph; the viewport is fixed.
- **No legend** explaining what the orange (cross-repo) lines mean.
- **No filters** — the user cannot focus on a single repo, a single kind (Function/Method/Class), or the cross-repo edges alone.
- **Hover detail is browser-default `<title>`** — not styled, single-line, fades fast.

The result is that the tab's value as a code-intelligence surface (the reason for the entire project) is undercut by the rendering. The hero recording shows the graph at its lamest.

## Goal

Make the Graph tab readable at federation scale: distinct colour per repo, name-on-hover with neighbour highlighting, zoom + pan via D3 zoom behaviour, a small minimap for orientation, and a filter chip strip for repo/kind/cross-repo-only.

The hero recording should re-render against the upgraded tab; the upgrade is the headline of "filters, colours, etc" that was requested.

## Approach

SPA-only change to `src/server/mcp/command_center/{app.js, styles.css, index.html}`. No server-side filter param. No tool / schema / Rust changes.

The visual encoding has **two independent axes**: node **shape** carries the kind (Function / Method / Class) and node **colour** carries the repo. Using two dimensions means a user can read "this is `bytes::Buf`, a Method" or "these are all the Functions in `tokio`" at a glance, without overloading either channel.

Three significant features land together because they share the same SVG layer + state model:

1. **Filter strip** above the canvas — one chip per repo (derived from the payload's `repo_id` set), three kind chips (Function / Method / Class — these toggle shape visibility, not just dim), and two toggle chips (Cross-repo only, Labels always on). Filters apply as CSS classes (`is-dim`, `is-hidden`) so the D3 simulation does not re-run on toggle.
2. **Two-axis visual encoding**:
   - **Shape per kind** — `Function` → circle (existing default), `Method` → diamond, `Class` → square. Rendered via D3 `d3.symbol()` (built-in `symbolCircle`, `symbolDiamond`, `symbolSquare`) with each node as a `<path>` element rather than `<circle>`. Shape is the primary readable channel — works in monochrome.
   - **Colour per repo** — five new CSS variables in `theme.css` (`--repo-a` … `--repo-e`); nodes get a class `graph-repo-<i>` based on the repos' alphabetical index in the payload (stable across re-renders). Colour is the secondary channel — redundant info for accessibility, primary channel for repo clustering.
   - **Edges stay orthogonal** — `cross_repo: true` → `var(--warn)` (orange); intra-repo → `var(--border-strong)`. Edge colour and node colour use different hues so the two channels don't fight.
3. **Zoom + minimap + hover focus** — D3 `zoom()` behaviour on a wrapping `<g class="graph-viewport">`. Minimap renders the un-transformed graph small (≈ 150 × 100 px) with a translucent viewport indicator. Hover on a node adds `.is-focus` to the node, `.is-neighbour` to its 1-hop neighbours, `.is-dim` to the rest. The `<title>` element gets promoted to a styled `<g class="graph-tooltip">` that follows the cursor.

The legend therefore has a **two-axis structure**: rows by repo (color) and columns by kind (shape), producing a 3 × N grid where N is the number of repos. Each cell is a small `<svg>` with one representative symbol of the right shape and colour, plus its label.

Why all three features land together:

- Filters without zoom are useless at 5 000 nodes — the user can filter to one repo but still can't see anything.
- Colours/shapes without filters give a coarse navigation by repo or kind, but filters + visual encoding together let the user focus and then read.
- Zoom without visual differentiation means every glyph still looks identical at any zoom level.
- Hover focus without shape/colour doesn't help the user find what they hovered; the dual encoding makes the focus legible.

Carving the three into separate PRs would deliver two of three vs. all-three; the all-in scope is the lowest-friction path.

### Why shape + colour, not two colours

The alternative encoding — a 5-repo × 3-kind = 15-colour palette — sounds denser but reads worse: 15 hues are impossible to disambiguate at a glance, even colour-blind-friendly palettes. The two-channel encoding (shape + colour) gives the same information density (3 × N) with much better legibility. It also survives a monochrome print or theme-switched view, which a pure-colour encoding wouldn't.


## Components

| File | Responsibility |
|---|---|
| `src/server/mcp/command_center/index.html` | Insert `<div class="graph-filter-bar">` and `<svg id="graph-minimap">` inside `#tab-graph`. The filter bar holds two rows (or two columns on narrow viewports): repo chips, then kind chips + the two toggle chips (`cross-repo-only`, `labels`). Keep the existing `<select id="graph-workspace">`, `<span id="graph-meta">`, `<div id="graph-empty">`, `<svg id="graph-canvas">`. |
| `src/server/mcp/command_center/styles.css` | New rules for `.graph-filter-bar`, `.graph-chip`, `.graph-chip.is-on`, `.graph-repo-a` … `.graph-repo-e`, `.graph-node--kind-Function` (circle), `.graph-node--kind-Method` (diamond), `.graph-node--kind-Class` (square), `.graph-node.is-focus`, `.graph-node.is-neighbour`, `.graph-node.is-dim`, `.graph-link.is-dim`, `.graph-viewport`, `.graph-minimap`, `.graph-minimap-frame`, `.graph-tooltip`. ~80 lines. |
| `src/server/mcp/command_center/app.js` | Rewrite `drawGraphSvg` (60 → ~280 lines) and add helpers `computeRepoPalette(graph) → Map<repoId, index>`, `nodeShape(kind) → d3 symbol`, `applyFilters(svg, state)` (DOM-toggle only), `paintMinimap(svg, viewport)` (renders a small copy + viewport rect), `wireZoom(svg)` (D3 `zoom()` on the viewport `<g>`), `paintLegend(graph, palette)` (two-axis grid), `onHover(svg, neighboursById)` (CSS class toggling). The `normalizeGraphPayload` helper stays as-is (still pure, still exported for tests). |
| `src/server/mcp/command_center/theme.css` | Add the five `--repo-a` … `--repo-e` tokens in both the dark and light themes. Tied to the existing `var(--accent)` / `var(--warn)` palette to keep the phosphor feel. |

## Filter state (closure-local, in `renderGraphTab`)

A plain object, reset on every `renderGraphTab` invocation:

```js
const filterState = {
  repos: new Set(graph.nodes.map(n => n.repo_id).filter(Boolean)),
  kinds: new Set(['Function', 'Method', 'Class']),
  crossRepoOnly: false,
  labelsAlwaysOn: false,
};
```

Toggles inside the filter bar mutate this state and call `applyFilters(svg, filterState)` (no D3 re-run, just class toggle).

## Filter application (CSS classes, not data filtering)

A node is "visible" if all of:

- `filterState.repos.has(d.repo_id)` (otherwise `.is-hidden` → `display: none`)
- `filterState.kinds.has(d.kind)` (otherwise `.is-hidden`)

A "cross-repo-only" toggle additionally marks nodes that have **zero** cross-repo edge in either direction as `.is-hidden`. We precompute a `Set<id>` of nodes that participate in cross-repo edges and reuse it on each apply.

A "labels always on" toggle removes the existing `nodes.length <= 150` threshold and always renders labels. The toggle defaults to off (matches today's behaviour).

`applyFilters(svg, state)` is a pure DOM-update function over the existing D3 selections; it does not re-run the force simulation. The simulation continues to tick at alpha decay in the background while the user toggles filters — its effect becomes invisible (everything `.is-dim` is opacity 0.15) but the layout is preserved.

## Colour palette

Five distinct hues:

| Token | Dark theme | Light theme | Notes |
|---|---|---|---|
| `--repo-a` | teal (#5fdfdf) | teal (#0f7c7c) | |
| `--repo-b` | warm amber (#f0b860) | warm amber (#a06400) | |
| `--repo-c` | magenta (#e07ee0) | magenta (#a020a0) | |
| `--repo-d` | olive (#a0d060) | olive (#407030) | |
| `--repo-e` | sky blue (#80b8ff) | sky blue (#1860c0) | |
| `--repo-fallback` | `var(--accent)` | `var(--accent)` | sixth-and-onward repos |

Repos are assigned an index by their first appearance in `graph.nodes[*].repo_id` (alphabetical at draw time, so the same repo gets the same colour across re-renders). The palette can hold up to five distinct repos; the sixth-and-onward reuses `--accent` so a multi-repo workspace (5+ repos) still has visual differentiation but doesn't demand a new colour system.

Cross-repo edges keep `stroke: var(--warn)` (orange); intra-repo edges keep `stroke: var(--border-strong)` (dim). The user can distinguish cross-repo lines at a glance.

## Shape vocabulary

Three shapes mapped from `n.kind` to D3's built-in `symbol()` constructors:

| Kind | D3 symbol | CSS class |
|---|---|---|
| `Function` | `d3.symbolCircle` | `.graph-node--kind-Function` |
| `Method` | `d3.symbolDiamond` | `.graph-node--kind-Method` |
| `Class` | `d3.symbolSquare` | `.graph-node--kind-Class` |
| anything else (defensive) | `d3.symbolCircle` | `.graph-node--kind-Function` |

Render path: each node becomes a `<path>` whose `d` attribute is `d3.symbol().size(...).type(...)()` (i.e. the SVG path-data string of the symbol centred on the node's `(x, y)`). Default symbol size: 5 × 5 logical units (similar area to the previous 5px-radius circle). On focus, symbol size grows; on neighbour, intermediate. The neighbouring CSS uses the `currentColor` keyword for the path fill so the per-repo `--repo-*` token drives colour and shape doesn't interfere.

D3 symbols are well-supported and don't introduce new dependencies (`assets/d3.v7.min.js` already has them — `d3.symbolCircle`, `d3.symbolDiamond`, `d3.symbolSquare` are part of D3 v3+ which `d3-selection` v7 carries). The existing `assets/d3.v7.min.js` is unchanged.

## Legend (two-axis)

Painted by `paintLegend(graph, palette)` as a small grid below the filter bar (or in a corner of the header strip — chosen at implementation time based on layout probe). Each row is a repo; each column is a kind:

```
                  Function  Method  Class
bytes (teal)        ○         ◆       □
tokio (amber)       ○         ◆       □
```

Cell rendering: a tiny `<svg>` per cell with a single `<path>` of the correct shape and colour, plus a text label. Cells whose (repo, kind) combination has zero nodes are still drawn (so the grid is rectangular and stable across re-renders) but rendered with reduced opacity to indicate "no data for this combination".

## Zoom + minimap

`d3.zoom().scaleExtent([0.2, 8])` on the main SVG:

- Wheel = zoom (clamped at 0.2–8).
- Drag empty space = pan.
- Drag a node = move the node (existing behaviour, preserved via `d3.drag` on the `<circle>` selections; the zoom and drag are wired so they don't conflict).

The zoom transform applies to a wrapping `<g class="graph-viewport">` containing nodes + links, not to the SVG itself, so the `<title>` tooltip and the minimap are unaffected.

A reset button in the filter bar returns the transform to `scale 1 / translate 0`.

**Minimap** (`<svg id="graph-minimap">` in the corner, ~150 × 100 px):

- Renders the un-transformed graph in miniature: each node a 1-pixel dot at its current force-simulated `(d.x, d.y)` (mapped to minimap coordinates via the canvas bounding box).
- Filters apply to the minimap too — a filtered-out node is invisible in both views.
- A translucent rectangle `<rect class="graph-minimap-frame">` indicates the current viewport (positioned via the zoom transform), with click-to-pan on the minimap itself.

`paintMinimap` runs on every filter change and every zoom change (cheap; ~100 line segments).

## Hover focus

Three CSS classes drive the hover behaviour:

- `.graph-node.is-focus` — radius 7, full colour (overrides per-repo colour tokens).
- `.graph-node.is-neighbour` — radius 6, current colour at opacity 0.85.
- `.graph-node.is-dim` — opacity 0.15 (everything else in the graph).
- `.graph-link.is-dim` — opacity 0.05 (all links fade when a node is focused; only edges incident on the focus or its neighbours stay at full colour).

Hover is wired via D3's `.on('mouseover', ...)` / `.on('mouseout', ...)`. A `neighboursById` `Map<id, Set<id>>` is precomputed from `graph.edges` at draw time so hover lookups are O(1).

The browser `<title>` tooltip is replaced with a styled `<g class="graph-tooltip">` that follows the mouse (via `mousemove`) and shows `name · repo · kind · path · degree`. This is a small d3 join; it's another ~30 lines.

## Error handling

- Filter state is closure-local to `renderGraphTab`, reset on every tab activation. No stale state across workspace switches.
- An empty filter (no repos enabled, or no kinds enabled) shows a one-line hint inside the canvas via `<text class="muted">Pick at least one repo to display.</text>` rather than an empty `<svg>` (which is jarring).
- Zoom and minimap degrade gracefully if `d3.zoom` is undefined (defensive check inside `wireZoom`): fall back to no zoom behaviour, minimap still renders but the viewport frame is hidden.
- The existing `renderGraphTabEmpty(state, list)` paths for "no workspace matches", "get_workspace_graph failed", etc. continue to apply — the new filter UI lives inside `#tab-graph` and only renders after the empty-state gates are passed.

## Testing

### Pure-helper tests in `tests/js/graph_tab.test.js`

Add four new pure helpers to `app.js`'s CommonJS export footer (matching the existing pattern at `app.js:1226-1233`):

```js
function applyFilters(graph, state) {
  // Pure: returns {visibleNodes, visibleEdges, hiddenNodeIds, hiddenEdgeIds}.
  // Used by both the SPA and unit tests.
}

function repoColour(repoId, repoIndexMap) {
  // Pure: returns 'graph-repo-a' | 'graph-repo-b' | ... | 'graph-repo-fallback'.
}

function nodeRadius(role /* 'focus'|'neighbour'|'default' */) {
  return role === 'focus' ? 7 : role === 'neighbour' ? 6 : 5;
}

function nodeShape(kind /* string */) {
  // Pure: returns 'circle' | 'diamond' | 'square' (D3 symbol type names).
  // Anything not in {'Function','Method','Class'} maps to 'circle'.
  return kind === 'Method' ? 'diamond'
       : kind === 'Class'  ? 'square'
       :                    'circle';
}
```

Tests cover:

- `applyFilters` with `crossRepoOnly: true` and a fixture of 10 nodes / 12 edges — only the cross-repo nodes / edges survive.
- `applyFilters` with one repo unselected — that repo's nodes / edges vanish.
- `applyFilters` with `labelsAlwaysOn: true` — the function still returns the same visible set; the labels threshold is purely a render-time decision.
- `applyFilters` with `kinds: new Set(['Function'])` — only Function nodes survive; Method and Class nodes vanish.
- `repoColour` round-trips through a 6-repo index map → the 6th repo gets `graph-repo-fallback`.
- `nodeShape` round-trips `Function → circle`, `Method → diamond`, `Class → square`, unknown → `circle`.

### SPA end-to-end tests in `tests/js/spa_e2e.test.js`

Add Graph-tab assertions:

- After the Graph tab renders, `#tab-graph .graph-filter-bar` has exactly `N+2` chips where `N` is the number of repos in the fixture. The 2 extra are `cross-repo-only` and `labels`.
- Clicking the first repo's filter chip toggles `.is-dim` on the chip and adds `.is-hidden` to that repo's nodes; clicking again removes both.
- Clicking the `Method` kind chip removes `.graph-node--kind-Method` rendering (or applies `.is-hidden` to Method-nodes' paths).
- The legend renders a `<rect>` or `<g class="graph-legend-cell">` per (repo, kind) pair.
- Dispatching `wheel` on `#graph-canvas` triggers a `transform` attribute on `g.graph-viewport`.

### Recorder driver in `tests/js/record_spa_demo.js`

The Graph-tab step's per-step `setTimeout` grows from 8 s → 10 s; filters + minimap + zoom + legend add a few hundred ms of layout work. The federation probe gate (`probeFederationCrossRepoGraph`) still catches catastrophic graph failures.

Additionally, the Graph-tab `waitForFunction` at `tests/js/record_spa_demo.js:335` (which checks for `circle > 0`) needs to update to `path > 0` (since nodes are now `<path>` elements, not `<circle>`). The timeout cap stays at 15 s.

### Recorder driver in `tests/js/record_spa_demo.js`

The Graph-tab step's per-step `setTimeout` grows from 8 s → 10 s; filters + minimap + zoom add a few hundred ms of layout work. The federation probe gate (`probeFederationCrossRepoGraph`) still catches catastrophic graph failures.

### Hero recording

Re-record after the change. The README hero GIF / MP4 / WebM / poster capture the upgraded graph tab; the existing 28-second driving time absorbs the 2-second growth.

## Recording budget update

| Recording step              | Was | Becomes |
|-----------------------------|-----|---------|
| Federation ready wait       | ≤ 90 s | ≤ 90 s |
| Overview tab hold           | 4 s | 4 s |
| Repos tab hold              | 4 s | 4 s |
| Query tab hold              | 6 s | 6 s |
| Tools tab hold              | 6 s | 6 s |
| Graph tab hold              | 8 s | 10 s |
| **Total driving time**      | ~28 s | ~30 s |

Recording still under the 45-second hero budget.

## Out of scope

- **Server-side filtering** (`get_workspace_graph?repo=…`) — filtering is pure-client. Server response stays the same shape.
- **Click-to-expand-subgraph** (pattern C in brainstorming) — the user can already see the whole graph with zoom; hidden-with-detail is a different mental model.
- **Search-by-name input** — YAGNI; hover-focus + filters cover the common paths.
- **Kind-by-repository cross-filter** — the filter bar grows naturally if needed later (chip stacks); not in this PR.
- **`find_anchors`-driven layout** — D3 force is fine; an anchor-driven layout is a future optimization.

## Verification gates before declaring done

1. `cargo test --test cli_surface` passes (commands table untouched).
2. `node --test tests/js/graph_tab.test.js` passes including the three new pure-helper tests.
3. `node tests/js/spa_e2e.test.js` passes including the Graph-tab filter and zoom assertions.
4. `bash scripts/smoke_federation_fixture.sh` passes (no fixture changes).
5. `make record-demo` produces the upgraded hero artifacts; frame check at t≈25 confirms the Graph tab now shows coloured nodes + filter chips + minimap (visually verifiable in the recording).
6. README troubleshooting check: `Status:` line still mentions `LAIN_REINDEX_TIMEOUT` defaults correctly.
7. Working tree clean apart from pre-existing untracked files.

## Risk register

| Risk | Mitigation |
|------|------------|
| D3 force simulation + D3 zoom conflict on node-drag | Existing `d3.drag()` is bound to the inner `<path>`/`<circle>` selection (the same handler logic works on either element); `d3.zoom()` is bound to the wrapping SVG. Drag on a node is captured before zoom; drag on empty space pans. Existing `app.js:888-897` pattern. |
| Filter-changes blow out the graph's force layout (visible flicker) | Filters apply as CSS classes, not data filtering. The D3 simulation does not re-run; class toggles are O(N) DOM updates, sub-frame. |
| Minimap repaints on every zoom and become a perf bottleneck | Minimap renders ≤ 100 tiny dots + 1 rect per frame; on a modern browser this is well under 1 ms. If it becomes a bottleneck, the paintMinimap can be debounced to 60 fps. |
| 5-repo palette is colour-blind-inconsiderate | The chosen hues (teal / amber / magenta / olive / sky-blue) cover the deuteranopia and protanopia spectrum per Wong, 2011; the cross-repo edges in `var(--warn)` add a second visual cue beyond colour (line stroke vs node fill); the **shape** axis adds a third independent cue for kind (circle / diamond / square). Three independent encoding channels. Acceptable for v1; a follow-up could expose a colour-blind-friendly palette via `var()`. |
| Hero recording's Graph frame longer than 8 s on slow host | Per-step `setTimeout` grows to 10 s. The 8 MB GIF target has 2 s of headroom in the existing ladder. |
| Theme switcher (paper/phosphor) breaks the new colour tokens | The new `--repo-*` tokens are defined per-theme in `theme.css` alongside the existing palette; both themes get a full set. |
| The `tests/cli_surface.rs` table drift | This change doesn't touch the `## The commands` section in README. The test still passes. |
| Recording pipeline regression | `tests/js/spa_e2e.test.js` exercises Graph tab end-to-end and now includes zoom + filter + shape assertions. If the upgrade breaks something, the e2e gate fails before the recording step is hit. |
| D3 symbol `size()` doesn't translate 1:1 from radius | A 5px-radius circle has area `π*25 ≈ 78.5`. `d3.symbol().size(...)` defaults to square area; we set size 64 (≈ 8×8 square area) which is closer to the existing 5px-radius circle visually. Tune by inspection during implementation; tests assert shape, not size. |
| Recorder's `circle > 0` selector (existing `record_spa_demo.js:335`) breaks when nodes become `<path>` | Update the selector to `path`. Implementation note in the test section above. |

## Files touched

Modified:

- `src/server/mcp/command_center/index.html` — insert filter bar + minimap slots inside `#tab-graph`.
- `src/server/mcp/command_center/styles.css` — add filter / chip / repo colour / focus / minimap / tooltip rules.
- `src/server/mcp/command_center/theme.css` — add `--repo-a` … `--repo-e` per-theme tokens.
- `src/server/mcp/command_center/app.js` — rewrite `drawGraphSvg` + add helpers; add new pure helpers to the CommonJS export footer.
- `tests/js/graph_tab.test.js` — three new pure-helper tests.
- `tests/js/spa_e2e.test.js` — three new Graph-tab assertions.
- `tests/js/record_spa_demo.js` — per-step Graph hold 8 s → 10 s.
- `docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}` — re-recorded hero artifacts.
- `README.md` — optional footer note about the upgraded Graph tab if the recording timestamps change (currently doesn't, since the only visible delta in the hero is the Graph-tab frame).

Untouched (explicitly):

- `src/server/**` (no Rust changes; `get_workspace_graph` and its 5 000-node / 10 000-edge caps stay exactly as they are).
- `src/cli/**`, `tests/**.rs`, `.github/workflows/**`.
- `docs/command-center.md`, `docs/QUICKSTART.md`, `docs/FEDERATION.md`, `docs/ARCHITECTURE.md`, `docs/USER_MANUAL.md`, `docs/TECHNICAL.md`, `docs/hooks.md`, `docs/hot-reload.md`, `docs/multiplayer.md`, `docs/query-language.md`, `docs/quickstart-tools.md`, `docs/REPOS_YAML.md`, `docs/INDEX.md`, `docs/CI.md`, `docs/opinions/**`, `docs/srs/**`, `docs/wish-list.md`.
- `scripts/**` (no fixture or recording-pipeline changes).
- The recording pipeline's existing parseArgs / federation probe / federation-timeout / env-var logic.
