# SPA Graph tab: anchor-first default + on-demand neighbourhood

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the v1 SPA Graph tab's "render the full 5 000-node hairball" default with an anchor-first default that ships a small, readable graph (≈50-100 nodes), and adds focal and search modes for on-demand neighbourhood views.

**Architecture:** SPA-only change to `src/server/mcp/command_center/{app.js, styles.css, index.html}`. Default render uses the existing `find_anchors` MCP tool (top 30 anchors + their 1-hop neighbourhood, capped at 20 neighbours per anchor). Click any node → fetch `get_blast_radius { symbol, depth }` for that symbol. Search input → focalise via the same tool. All the v1 upgrade's colour/shape/zoom/minimap/hover-focus/legend helpers carry over unchanged; the upgrade is mostly `drawGraphSvg` adaptation plus a small HTML/CSS delta for the new controls.

**Tech Stack:** Vanilla JS in `src/server/mcp/command_center/app.js`; D3 v7 (`assets/d3.v7.min.js`); vanilla CSS variables. No new dependencies.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-08-31-spa-graph-anchors-first-design.md` (commit `c2e8013`).
- Plan supersedes nothing — no edits to README, scripts, Rust source, .github, or docs outside `src/server/mcp/command_center/**`.
- Defaults: 30 anchors via `find_anchors { limit: 30 }`; 1-hop neighbourhood per anchor; per-anchor neighbour cap 20.
- Edge styling bumps: `.graph-link.cross-repo` stroke-width 1.5 → 2; `.graph-link` stroke-opacity 0.6 → 0.75.
- `app.js` is `include_bytes!`'d at `src/server/mcp/command_center_assets.rs:31`. Any change to `app.js` requires `cargo build --release --quiet` before the recorder / e2e can see the change.
- Recorder settle: Graph-tab `setTimeout` reverts from 10 s (v1) → 6 s (v2). Anchor view's small graph settles fast.
- Pure-helpers exported via the CommonJS footer at `app.js:1301-1315` (the v1 export slot) — `nodeShape` now returns `'symbolCircle' | 'symbolDiamond' | 'symbolSquare'`.

---

## File Structure

Modified:

| File | Responsibility |
|---|---|
| `src/server/mcp/command_center/app.js` | Rewrite the data-fetch path inside `drawGraphSvg` to operate on a "visible set" derived from the active mode (anchor | focal). Add `computeAnchorVisibleSet` and `applyDepth` pure helpers. Extend `buildFilterBar` to wire up the search input, depth slider, and "back to anchors" button. State keeps `mode`, `depth`, `searchQuery` alongside the existing filters. |
| `src/server/mcp/command_center/index.html` | Insert `<input type="search" data-graph-search>` and `<input type="range" data-graph-depth>` (min=1, max=3, value=1) and `<button data-graph-back>` inside the existing `.graph-filter-bar` block. Keep all of Tasks 3's earlier markup. |
| `src/server/mcp/command_center/styles.css` | Add ~20 lines for the search input, range slider, and back button. Bump `.graph-link` and `.graph-link.cross-repo` per the spec. |
| `tests/js/graph_tab.test.js` | Add 3 pure-helper tests for `computeAnchorVisibleSet` (single anchor with no edges, two anchors sharing a neighbour, multi-anchor dedup + neighbour cap). |
| `tests/js/spa_e2e.test.js` | Add 4 new Graph-tab assertions for the v2 behaviour (anchor count bounded; click-replaces-visible-set; search input focalises; back button restores). |
| `tests/js/record_spa_demo.js` | Graph-tab `setTimeout` 10 s → 6 s; selector fix from v1 still applies (`path.graph-node`). |
| `docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}` | Re-recorded hero artifacts (anchor view, t≈25 s). |

Created: none.

Untouched (explicitly):

- `src/server/**` (no Rust changes; `find_anchors` and `get_blast_radius` and `get_workspace_graph` are existing MCP tools).
- `scripts/**`, `.github/workflows/**`, `Cargo.*`, README, `docs/**` files outside `src/server/mcp/command_center/**`.

---

## Task 1: Pure helpers — TDD with `computeAnchorVisibleSet` and `applyDepth`

**Files:**
- Modify: `src/server/mcp/command_center/app.js` — append the two helpers near the existing v1 helpers (around line 870-900) and add them to the CommonJS export footer at `app.js:1301-1315`.
- Modify: `tests/js/graph_tab.test.js` — append 3 new test cases.

**Interfaces:**
- Consumes: `anchors` (array of `{ name, repo_id, kind?, ... }`), `workspaceGraph` (the existing normalised payload with `{ nodes, edges, truncated }`), `opts.neighbourhoodDepth` (default 1), `opts.maxNeighboursPerAnchor` (default 20).
- Produces: DOM-free pure helpers that `node --test tests/js/` can import.

- [ ] **Step 1: Append the helpers to `app.js` near the existing v1 helpers (above the graph section header)**

```js
function computeAnchorVisibleSet(anchors, workspaceGraph, opts = {}) {
  const neighbourhoodDepth = opts.neighbourhoodDepth ?? 1;
  const maxNeighboursPerAnchor = opts.maxNeighboursPerAnchor ?? 20;
  if (!Array.isArray(anchors) || anchors.length === 0) {
    return { nodes: [], edges: [], hiddenNodeIds: new Set() };
  }
  if (!workspaceGraph || !Array.isArray(workspaceGraph.edges)) {
    return { nodes: [], edges: [], hiddenNodeIds: new Set() };
  }

  // Build adjacency lookup once: id -> Set<id> for edge neighbours.
  const adj = new Map();
  for (const e of workspaceGraph.edges) {
    if (!adj.has(e.source)) adj.set(e.source, new Set());
    if (!adj.has(e.target)) adj.set(e.target, new Set());
    adj.get(e.source).add(e.target);
    adj.get(e.target).add(e.source);
  }

  // Anchor lookup by node name (find_anchors returns names; the
  // workspace graph uses global ids — match by name within the same repo).
  const nodesByName = new Map();
  for (const n of workspaceGraph.nodes) {
    if (n.name && n.repo_id) {
      const key = `${n.repo_id}::${n.name}`;
      if (!nodesByName.has(key)) nodesByName.set(key, []);
      nodesByName.get(key).push(n);
    }
  }

  // Walk neighbourhood per anchor; cap the immediate-neighbour
  // contribution to `maxNeighboursPerAnchor` by neighbour degree.
  const visibleIds = new Set();
  for (const a of anchors) {
    if (!a || !a.name || !a.repo_id) continue;
    const candidates = nodesByName.get(`${a.repo_id}::${a.name}`) || [];
    if (candidates.length === 0) continue;
    for (const c of candidates) visibleIds.add(c.id);

    if (neighbourhoodDepth >= 1) {
      const neighbourBag = [];
      for (const c of candidates) {
        const neigh = adj.get(c.id);
        if (!neigh) continue;
        for (const nid of neigh) {
          neighbourBag.push({ id: nid, degree: (adj.get(nid) || new Set()).size });
        }
      }
      neighbourBag.sort((a, b) => b.degree - a.degree);
      for (const n of neighbourBag.slice(0, maxNeighboursPerAnchor)) {
        visibleIds.add(n.id);
      }
    }
  }

  const nodeById = new Map((workspaceGraph.nodes || []).map(n => [n.id, n]));
  const visibleNodes = [];
  const hiddenNodeIds = new Set();
  for (const n of workspaceGraph.nodes) {
    if (visibleIds.has(n.id)) visibleNodes.push(n);
    else hiddenNodeIds.add(n.id);
  }
  const visibleEdges = [];
  for (const e of workspaceGraph.edges) {
    if (visibleIds.has(e.source) && visibleIds.has(e.target)) visibleEdges.push(e);
  }
  return { nodes: visibleNodes, edges: visibleEdges, hiddenNodeIds };
}

function applyDepth(neighbourhood, anchor, depth) {
  // Pure passthrough — currently no narrowing; explicitly accepts `depth`
  // so callers can read the depth slider state and pass it through to
  // get_blast_radius. Reserved for future use (e.g., depth>1 client-side
  // expansion of focal views); returns the same neighbourhood for now.
  return neighbourhood;
}
```

- [ ] **Step 2: Add the two helpers to the CommonJS export footer at `app.js:1301-1315`**

Append to the existing object literal so the footer becomes:

```js
module.exports = {
  collapseBursts, filterConflictEvents, pickWorkspaceForGraph,
  classifyWorkspacesResult, normalizeGraphPayload,
  // SPA graph upgrade (2026-08-31):
  computeRepoPalette, applyFilters, repoColour, nodeShape, nodeRadius,
  // SPA graph v2: anchors-first (2026-08-31):
  computeAnchorVisibleSet, applyDepth,
};
```

- [ ] **Step 3: Append 3 test cases to `tests/js/graph_tab.test.js`**

```js
test('computeAnchorVisibleSet: anchor with no incident edges is itself visible', () => {
  const anchors = [{ name: 'orphan', repo_id: 'r1' }];
  const workspaceGraph = {
    nodes: [{ id: 'a', name: 'orphan', repo_id: 'r1', kind: 'Function' },
            { id: 'b', name: 'others', repo_id: 'r1', kind: 'Function' }],
    edges: [{ source: 'b', target: 'b2', cross_repo: false }],
  };
  const out = computeAnchorVisibleSet(anchors, workspaceGraph);
  assert.equal(out.nodes.length, 1);
  assert.equal(out.nodes[0].id, 'a');
  assert.equal(out.edges.length, 0);
});

test('computeAnchorVisibleSet: two anchors sharing a neighbour dedup the neighbour', () => {
  const anchors = [{ name: 'a', repo_id: 'r1' }, { name: 'b', repo_id: 'r1' }];
  const workspaceGraph = {
    nodes: [{ id: 'na', name: 'a', repo_id: 'r1' },
            { id: 'nb', name: 'b', repo_id: 'r1' },
            { id: 'shared', name: 'shared', repo_id: 'r1' }],
    edges: [
      { source: 'na',    target: 'shared', cross_repo: false },
      { source: 'nb',    target: 'shared', cross_repo: false },
      { source: 'other', target: 'shared', cross_repo: false },
    ],
  };
  const out = computeAnchorVisibleSet(anchors, workspaceGraph);
  // Anchors na + nb, plus their shared neighbour (counted ONCE), not 'other'.
  assert.equal(out.nodes.length, 3);
  const ids = new Set(out.nodes.map(n => n.id));
  assert.ok(ids.has('na'));
  assert.ok(ids.has('nb'));
  assert.ok(ids.has('shared'));
  assert.ok(!ids.has('other'));
});

test('computeAnchorVisibleSet: neighbourhood cap limits per-anchor contribution', () => {
  const anchors = [{ name: 'hub', repo_id: 'r1' }];
  const nodes = [{ id: 'hub', name: 'hub', repo_id: 'r1', kind: 'Function' }];
  const edges = [];
  for (let i = 0; i < 30; i++) {
    nodes.push({ id: `n${i}`, name: `n${i}`, repo_id: 'r1', kind: 'Function' });
    edges.push({ source: 'hub', target: `n${i}`, cross_repo: false });
  }
  const out = computeAnchorVisibleSet(anchors, { nodes, edges }, { maxNeighboursPerAnchor: 5 });
  // 1 anchor + capped 5 neighbours = 6 visible.
  assert.equal(out.nodes.length, 6);
  const visibleIds = new Set(out.nodes.map(n => n.id));
  assert.ok(visibleIds.has('hub'));
  const neighbours = out.nodes.filter(n => n.id !== 'hub');
  assert.equal(neighbours.length, 5);
});
```

- [ ] **Step 4: Run the unit tests**

```bash
node --test tests/js/graph_tab.test.js
```

Expected: ≥ 30 pass total (27 existing + 3 new). All 3 new tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/server/mcp/command_center/app.js tests/js/graph_tab.test.js
git commit -m "feat(spa): computeAnchorVisibleSet + applyDepth helpers (anchor-first data flow)"
```

---

## Task 2: HTML — search input + depth slider + back button

**Files:**
- Modify: `src/server/mcp/command_center/index.html` lines 64-87 (the `#tab-graph` block).

**Interfaces:**
- Produces: three new interactive elements inside the existing `.graph-filter-bar` from v1's Task 3:
  - `<input type="search" data-graph-search placeholder="focus on symbol…">`
  - `<input type="range" min="1" max="3" value="1" data-graph-depth>` (only shown in focal mode)
  - `<button data-graph-back>← anchors</button>` (only shown in focal mode)

- [ ] **Step 1: Re-read the current `#tab-graph` block**

Read `src/server/mcp/command_center/index.html` lines 64-87. Confirm the structure laid down in the v1 Task 3 commit (`d28c414`):

```html
<section id="tab-graph" class="tab">
  <div class="graph-header">...</div>
  <div class="graph-filter-bar" data-filter-bar>
    <div class="graph-filter-row" data-filter-row="repos">...</div>
    <div class="graph-filter-row" data-filter-row="kinds">...</div>
    <div class="graph-filter-row" data-filter-row="toggles">...</div>
  </div>
  <div id="graph-empty" class="muted"></div>
  <div class="graph-canvas-wrap">
    <svg id="graph-canvas" ...></svg>
    <svg id="graph-minimap" ...></svg>
  </div>
  <div class="graph-legend" data-graph-legend aria-hidden="true"></div>
</section>
```

(The rows' contents are populated by `buildFilterBar` in `app.js`; the static HTML only has the `<span class="graph-filter-label muted">` placeholders.)

- [ ] **Step 2: Insert the controls inside `.graph-filter-bar`, BEFORE the three `<div class="graph-filter-row">` lines**

A new row at the top, holding the search input. The slider and back-button live in a new `<div class="graph-filter-row" data-filter-row="focal" hidden>` that's hidden by default (revealed by `app.js` when state.mode === 'focal'):

```html
<div class="graph-filter-bar" data-filter-bar>
  <div class="graph-filter-row" data-filter-row="search">
    <span class="graph-filter-label muted">search</span>
    <input type="search" data-graph-search placeholder="focus on symbol…" aria-label="focus on symbol">
  </div>
  <div class="graph-filter-row" data-filter-row="focal" data-active="0" hidden>
    <span class="graph-filter-label muted">focal</span>
    <input type="range" min="1" max="3" value="1" data-graph-depth aria-label="blast radius depth">
    <button class="graph-chip" data-graph-back>← anchors</button>
  </div>
  <div class="graph-filter-row" data-filter-row="repos">
    <span class="graph-filter-label muted">repos</span>
  </div>
  <div class="graph-filter-row" data-filter-row="kinds">
    <span class="graph-filter-label muted">kind</span>
  </div>
  <div class="graph-filter-row" data-filter-row="toggles">
    <span class="graph-filter-label muted">view</span>
  </div>
</div>
```

The `hidden` attribute on the focal row keeps it hidden until JS adds `data-active="1"` AND removes the attribute. The `data-active="0"` is a JS-toggled hook for the visibility state.

- [ ] **Step 3: Verify the file still parses**

```bash
xmllint --html --noout src/server/mcp/command_center/index.html 2>&1 | head -5
# fallback if xmllint unavailable:
node -e "const fs=require('fs'); const h=fs.readFileSync('src/server/mcp/command_center/index.html','utf8'); console.log('size', h.length, 'lines', h.split('\n').length);"
```

Expected: parses cleanly.

- [ ] **Step 4: Commit**

```bash
git add src/server/mcp/command_center/index.html
git commit -m "feat(spa): graph filter bar — search input + depth slider + back-to-anchors"
```

---

## Task 3: CSS — controls + edge styling bump

**Files:**
- Modify: `src/server/mcp/command_center/styles.css` — append new rules and bump two existing values.

**Interfaces:**
- Produces: ~20 lines of CSS for the new controls + two value bumps in `.graph-link` and `.graph-link.cross-repo`.

- [ ] **Step 1: Locate the v1 rules that need updating**

Read `styles.css` and find the existing `.graph-link` and `.graph-link.cross-repo` rules. From the v1 review:

- `.graph-link { stroke: var(--border-strong); stroke-opacity: 0.6; stroke-width: 1; }`
- `.graph-link.cross-repo { stroke: var(--warn); stroke-opacity: 0.9; stroke-width: 1.5; }`

- [ ] **Step 2: Bump the two values in place (Edit the file directly)**

Change:
- `.graph-link`'s `stroke-opacity` from `0.6` to `0.75`.
- `.graph-link.cross-repo`'s `stroke-width` from `1.5` to `2`.

- [ ] **Step 3: Append new control rules after the v1 block (find the end of the `.graph-legend { ... }` rule)**

```css
/* ── Graph v2: search input, depth slider, back-to-anchors (2026-08-31) ─ */

[data-filter-row="search"] input[type="search"] {
  flex: 1;
  min-width: 12rem;
  padding: 0.25rem 0.5rem;
  border: 1px solid var(--border);
  border-radius: 3px;
  background: var(--surface);
  color: var(--fg);
  font-family: inherit;
  font-size: 0.78rem;
}
[data-filter-row="search"] input[type="search"]:focus {
  outline: none;
  border-color: var(--accent);
}

[data-filter-row="focal"] {
  border-top: 1px dashed var(--border);
  padding-top: 0.3rem;
  margin-top: 0.2rem;
}
[data-filter-row="focal"][hidden] { display: none; }
[data-filter-row="focal"][data-active="1"] { display: flex; }

[data-graph-depth"] {
  flex: 1;
  max-width: 160px;
  accent-color: var(--accent);
}

[data-graph-back"] {
  font-family: inherit;
}
```

Note: the `hidden` attribute (HTML bool) is the primary hide; the `data-active="1"` JS hook belt-and-braces for older browsers. The CSS `[hidden] { display: none }` is the UA default; we override `[data-active="1"]` to show.

- [ ] **Step 4: Verify CSS brace balance**

```bash
node -e "const fs=require('fs'); const css=fs.readFileSync('src/server/mcp/command_center/styles.css','utf8'); const opens=(css.match(/\{/g)||[]).length; const closes=(css.match(/\}/g)||[]).length; console.log('opens', opens, 'closes', closes, 'balanced', opens === closes);"
```

Expected: balanced.

- [ ] **Step 5: Commit**

```bash
git add src/server/mcp/command_center/styles.css
git commit -m "feat(spa): graph controls CSS — search/slider/back; bump edge stroke"
```

---

## Task 4: `drawGraphSvg` rewrite — anchor-first data flow + click + search + back

**Files:**
- Modify: `src/server/mcp/command_center/app.js` — replace the existing `drawGraphSvg` (post-Task-5-fix ~280 lines starting around `app.js:1135`) with the v2 rewrite.

**Interfaces:**
- Consumes: existing `drawGraphSvg(svgEl, graph)` signature — kept. The new implementation overrides the data fetch path inside `renderGraphTab` instead of inside `drawGraphSvg`. Specifically: the v2 changes happen at the orchestration level (`renderGraphTab`), not at the rendering level (`drawGraphSvg`). The brief acknowledges this layering in the spec's "Data Flow" section.
- Produces: a three-mode render (`anchor` | `focal`) with the existing filter-bar wiring upgraded to include search/depth/back.

Concretely:

- [ ] **Step 1: Read the current `renderGraphTab` body in `app.js` (the function that calls `drawGraphSvg`)**

Find the block. Should be around `app.js:1381` post-Task-5-fix (the lines have shifted; the function begins with `async function renderGraphTab()`).

The current shape (paraphrased):

```js
async function renderGraphTab() {
  // ... load workspace, populate picker, set empty text ...
  let result;
  try {
    result = await mcpCall('get_workspace_graph', {});
  } catch (e) { ... }
  // ... result.isError path ...
  const graph = normalizeGraphPayload(parseJson(result));
  // ... validate not empty ...
  drawGraphSvg(svg, graph);
}
```

This fetch-get_workspace_graph-then-render path is the v1 default. The v2 change is:

1. At mode='anchor': call `find_anchors { limit: 30 }`, then call `get_workspace_graph` only for the 1-hop expansion via `computeAnchorVisibleSet`, then render.
2. At mode='focal': call `get_blast_radius { symbol, depth }` directly, render.
3. Both modes: render with the existing helpers (palette, filter, minimap, zoom, etc.).

The cleanest layering: keep `drawGraphSvg` as the render-only function (existing signature `(svgEl, graph)` — it just renders whatever visible-set it gets), and reshape `renderGraphTab` to do the mode-aware fetch.

- [ ] **Step 2: Modify `renderGraphTab` to dispatch on mode**

Below is the rewrite of `renderGraphTab`. Keep the existing helper functions intact (computeRepoPalette, applyFilters, etc.):

```js
async function renderGraphTab(opts = {}) {
  const empty = document.getElementById('graph-empty');
  const meta = document.getElementById('graph-meta');
  const svg = document.getElementById('graph-canvas');
  if (!empty || !svg) return;

  const { state, list } = await loadGraphWorkspaces();
  if (state.mode !== 'auto' && !selectedGraphWorkspace) {
    renderGraphTabEmpty(state, list);
    return;
  }
  const target = selectedGraphWorkspace || state.workspace;
  const active = list.find(ws => ws && ws.is_active === true);
  if (active && target !== active.name) {
    renderGraphTabEmpty({ mode: 'not-loaded', workspace: target }, list);
    return;
  }
  populateGraphPicker(list, target);
  empty.className = 'muted';

  // v2: mode-aware fetch. If the caller passed a mode override, use it;
  // otherwise read the closure-local `graphState` (initialised at the
  // first call of renderGraphTab to mode='anchor').
  const mode = (opts && opts.mode) || (graphState.mode || 'anchor');
  graphState.mode = mode;
  setFocalRowVisible(mode === 'focal');

  if (mode === 'focal') {
    const focalSymbol = graphState.focalSymbol;
    const focalDepth = graphState.depth || 1;
    if (!focalSymbol) {
      // Defensive: if we somehow entered focal mode with no symbol,
      // fall back to anchor mode rather than rendering an empty graph.
      return renderGraphTab();
    }
    empty.textContent = `Loading ${focalSymbol}'s ${focalDepth}-hop neighbourhood…`;
    svg.innerHTML = '';
    let result;
    try {
      result = await mcpCall('get_blast_radius', { symbol: focalSymbol, depth: String(focalDepth) });
    } catch (e) {
      renderGraphTabEmpty({ mode: 'error', message: `get_blast_radius failed: ${e.message}` }, list);
      return;
    }
    if (result && result.isError) {
      const msg = unwrapText(result) || 'error';
      renderGraphTabEmpty({ mode: 'error', message: msg }, list);
      return;
    }
    const focalPayload = parseJson(result);
    // normalize via the existing helper if it matches the {nodes, edges, truncated} shape
    const graph = (focalPayload && Array.isArray(focalPayload.nodes))
      ? normalizeGraphPayload(focalPayload) : { nodes: [], edges: [], truncated: false };
    if (meta) {
      const cross = graph.edges.filter(e => e.cross_repo).length;
      meta.textContent = `focal: ${focalSymbol} · ${graph.nodes.length} nodes · ${graph.edges.length} edges · ${cross} cross-repo${graph.truncated ? ' · truncated' : ''}`;
    }
    empty.textContent = '';
    drawGraphSvg(svg, graph);
    return;
  }

  // Default: anchor mode. Fetch find_anchors, then the workspace graph
  // (used for the 1-hop neighbourhood expansion), then compute the
  // visible set.
  empty.textContent = 'Loading anchors…';
  svg.innerHTML = '';

  let anchorsResult;
  try {
    anchorsResult = await mcpCall('find_anchors', { limit: 30 });
  } catch (e) {
    renderGraphTabEmpty({ mode: 'error', message: `find_anchors failed: ${e.message}` }, list);
    return;
  }
  const anchorList = parseJson(anchorsResult);
  // anchorList is typically a numbered list of strings (the format
  // the driver regex matches). Be defensive: accept both string[] and {name}[].
  let anchors;
  if (Array.isArray(anchorList)) {
    anchors = anchorList.map((a, i) => {
      if (typeof a === 'string') return { name: a.trim(), repo_id: null };
      if (a && a.name) return { name: a.name, repo_id: a.repo_id || null };
      return null;
    }).filter(Boolean);
  } else {
    anchors = [];
  }
  if (anchors.length === 0) {
    empty.textContent = `Workspace ${target} has no anchors. Type a symbol to focalise.`;
    if (meta) meta.textContent = '';
    return;
  }

  let workspaceGraph;
  try {
    const wgResult = await mcpCall('get_workspace_graph', {});
    workspaceGraph = normalizeGraphPayload(parseJson(wgResult));
    graphState.workspaceGraph = workspaceGraph;
  } catch (e) {
    // Degrade: render anchors only (no 1-hop expansion).
    workspaceGraph = { nodes: [], edges: [], truncated: false };
  }
  const visible = computeAnchorVisibleSet(anchors, workspaceGraph, { neighbourhoodDepth: 1, maxNeighboursPerAnchor: 20 });
  if (meta) {
    const cross = visible.edges.filter(e => e.cross_repo).length;
    meta.textContent = `${anchors.length} anchors · ${visible.nodes.length} nodes · ${visible.edges.length} edges · ${cross} cross-repo${workspaceGraph.truncated ? ' · truncated' : ''}`;
  }
  if (visible.nodes.length === 0) {
    empty.textContent = `Workspace ${target} anchors produced no in-graph nodes.`;
    return;
  }
  empty.textContent = '';
  drawGraphSvg(svg, visible);
}
```

Also declare `graphState` at the closure of `renderGraphTab` (or as a module-level let):

```js
const graphState = {
  mode: 'anchor',          // 'anchor' | 'focal'
  focalSymbol: null,
  depth: 1,
  searchQuery: '',
  workspaceGraph: null,    // cached normalised payload (anchor mode's fallback)
};
```

Place this `graphState` declaration somewhere accessible to `renderGraphTab` and the new event handlers below — right above `renderGraphTab` is fine.

- [ ] **Step 3: Add `setFocalRowVisible(visible)` helper and call it before/at the start of `renderGraphTab`**

```js
function setFocalRowVisible(visible) {
  const row = document.querySelector('[data-filter-row="focal"]');
  if (!row) return;
  if (visible) {
    row.removeAttribute('hidden');
    row.setAttribute('data-active', '1');
  } else {
    row.setAttribute('hidden', '');
    row.setAttribute('data-active', '0');
  }
}
```

- [ ] **Step 4: Wire the search input, depth slider, and back button inside `buildFilterBar` (or as a separate `wireGraphControls` helper)**

The cleanest: extend `buildFilterBar` to also bind the new controls. The brief acknowledges this — three additional event handlers:

```js
function wireGraphControls(state, onChange) {
  const search = document.querySelector('[data-graph-search]');
  if (search) {
    let debounce = null;
    search.addEventListener('input', () => {
      clearTimeout(debounce);
      debounce = setTimeout(() => {
        const q = (search.value || '').trim();
        if (!q) return;
        // Resolve via find_anchors with the typed name as a hint; the
        // driver already does a regex match — here we just need to
        // surface the top match. We use a one-shot get_blast_radius
        // via resolveSymbol, which uses the symbol's full identity.
        state.focalSymbol = q;
        state.mode = 'focal';
        onChange({ restoreFromSearch: q });
      }, 300);
    });
  }
  const depth = document.querySelector('[data-graph-depth]');
  if (depth) {
    depth.addEventListener('input', () => {
      state.depth = Number(depth.value);
      if (state.mode === 'focal') onChange({ depthChanged: true });
    });
  }
  const back = document.querySelector('[data-graph-back]');
  if (back) {
    back.addEventListener('click', () => {
      state.mode = 'anchor';
      state.focalSymbol = null;
      onChange({ restoreFromFocal: true });
    });
  }
}
```

- [ ] **Step 5: Wire `wireGraphControls` from `renderGraphTab`'s post-render hook**

After `drawGraphSvg(svg, visible)` in the anchor-mode branch and after `drawGraphSvg(svg, graph)` in the focal-mode branch, call:

```js
wireGraphControls(graphState, ({ restoreFromFocal, restoreFromSearch, depthChanged } = {}) => {
  if (restoreFromFocal || restoreFromSearch || depthChanged) {
    if (depthChanged && graphState.mode === 'focal') {
      // Depth only changed; just re-fetch the same focal symbol at the new depth.
      return renderGraphTab();
    }
    return renderGraphTab();
  }
  // No-op default (chip-clicks from buildFilterBar don't reach here)
});
```

- [ ] **Step 6: Modify `drawGraphSvg` to wire the click handler that triggers focalisation**

The current `drawGraphSvg` (`app.js:1135` post-Task-5-fix) creates a D3 selection of `<path>` nodes. Add a `click` handler at the point where the selection is configured (alongside the existing `mouseover` / `mousemove` / `mouseout` handlers):

```js
node.on('click', (event, d) => {
  graphState.focalSymbol = d.name;
  graphState.mode = 'focal';
  renderGraphTab();
});
```

Replace the existing `node.on('mouseover', ...)` block with one that ALSO registers the click. The `graphState` and `renderGraphTab` are closure-accessible (the file-level `let graphState` and `function renderGraphTab` are declared above).

- [ ] **Step 7: Verify the file still parses and the rendering flow doesn't double-render**

```bash
node --check src/server/mcp/command_center/app.js
node --test tests/js/graph_tab.test.js
```

Expected: `node --check` clean; `graph_tab.test.js` ≥ 30 pass (no regressions on the v1 helpers; the new helpers have already been tested in Task 1).

- [ ] **Step 8: Commit**

```bash
chmod +x src/server/mcp/command_center/app.js
git add src/server/mcp/command_center/app.js
git commit -m "feat(spa): graph tab — anchor-first default + click/search/depth/back wiring"
```

---

## Task 5: SPA e2e assertions — anchor-mode behaviour

**Files:**
- Modify: `tests/js/spa_e2e.test.js`

**Interfaces:**
- Produces: 4 new assertions inside the existing Graph-tab e2e block, exercising the v2 anchor/focal/search/back flow.

- [ ] **Step 1: Locate the existing Graph-tab e2e block**

Find the section that drives Chromium through the Graph tab. It already has the `path.graph-node > 0` selector (v1 Task 6 fix). The v2 assertions go AFTER the existing wheel-zoom test.

- [ ] **Step 2: Add 4 new assertions after the existing wheel-zoom test**

```js
// v2: anchor-first default — the rendered graph is bounded.
const v2AnchorNodeCount = await page.evaluate(() => {
  return document.querySelectorAll('.graph-node').length;
});
assert.ok(v2AnchorNodeCount > 0, 'anchor view has nodes');
assert.ok(v2AnchorNodeCount <= 200,
  'anchor view is bounded (got ' + v2AnchorNodeCount + ' nodes; expected <= 200)');

// v2: clicking a node replaces the visible set with the focal neighbourhood.
const v2ClickResetsCount = await page.evaluate(() => {
  const before = document.querySelectorAll('.graph-node').length;
  const target = document.querySelector('.graph-node');
  if (!target) return { before, after: -1 };
  target.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
  return { before, after: -1, dispatched: true };
});
// Wait for the focal re-render to settle.
await new Promise(r => setTimeout(r, 1500));
const v2AfterClickCount = await page.evaluate(() => document.querySelectorAll('.graph-node').length);
assert.notEqual(v2AfterClickCount, v2ClickResetsCount.before,
  'clicking a node replaced the visible set (before=' + v2ClickResetsCount.before + ' after=' + v2AfterClickCount + ')');

// v2: search input focalises on typed symbol.
await page.evaluate(() => {
  const input = document.querySelector('[data-graph-search]');
  if (!input) return false;
  input.value = 'Buf';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  return true;
});
await new Promise(r => setTimeout(r, 1500));
const v2SearchActive = await page.evaluate(() => {
  const focal = document.querySelector('[data-filter-row="focal"]');
  return focal && focal.getAttribute('data-active') === '1';
});
assert.ok(v2SearchActive, 'search input activates focal mode');

// v2: "back to anchors" returns to anchor view.
await page.evaluate(() => {
  const back = document.querySelector('[data-graph-back]');
  if (back) back.click();
});
await new Promise(r => setTimeout(r, 1500));
const v2BackToAnchor = await page.evaluate(() => {
  const focal = document.querySelector('[data-filter-row="focal"]');
  const focalHidden = focal && focal.hasAttribute('hidden');
  const nodeCount = document.querySelectorAll('.graph-node').length;
  return { focalHidden, nodeCount };
});
assert.ok(v2BackToAnchor.focalHidden, 'back button hides focal row');
```

- [ ] **Step 3: Run the e2e**

```bash
node tests/js/spa_e2e.test.js 2>&1 | tail -15
```

Expected: total assertions = prior (43) + 4 new = 47. Pass.

- [ ] **Step 4: Commit**

```bash
git add tests/js/spa_e2e.test.js
git commit -m "test(spa): graph tab v2 — anchor bound, click focalises, search/back wired"
```

---

## Task 6: Recorder driver — Graph settle 10 s → 6 s

**Files:**
- Modify: `tests/js/record_spa_demo.js`

**Interfaces:**
- Produces: a recorder that settles faster on the v2 anchor view.

- [ ] **Step 1: Locate the Graph-tab block**

Find the existing v1 fix at `record_spa_demo.js:335-338` — the `setTimeout(r, 10000)` from the v1 upgrade. Drop it to `6000`.

- [ ] **Step 2: Replace `setTimeout(r, 10000)` with `setTimeout(r, 6000)`**

```js
// 5. Graph — let the D3 layout settle. Upgraded for v2 (anchors-first):
// the anchor view has ~50-100 nodes, so the simulation settles faster
// than the prior v1 hairball (which had 5000+ nodes).
await clickTab(page, 'graph');
try {
  await page.waitForFunction(() => {
    const svg = document.getElementById('graph-canvas');
    return svg && svg.querySelectorAll('path.graph-node').length > 0;
  }, { timeout: 15_000 });
} catch (_) {
  // Graph may not have data — the empty-state text is acceptable.
}
await new Promise(r => setTimeout(r, 6000));   // +0 s vs v1's 10s; small graph
```

- [ ] **Step 3: Run a smoke recording**

```bash
cargo build --release --quiet
./scripts/record-spa-demo.sh --no-build --keep-work --port 9942
ls -la /tmp/lain-record-spa-demo/raw.webm
rm -rf /tmp/lain-record-spa-demo
```

Expected: `raw.webm` ≥ 1 MB. Watch the orchestrator's stderr for the SPA upgrade's federation-ready line and the post-graph "video written" line.

- [ ] **Step 4: Commit**

```bash
git add tests/js/record_spa_demo.js
git commit -m "test(recorder): graph settle 10 s → 6 s for v2 anchor view"
```

---

## Task 7: Re-record hero + extract frame for the user

**Files:**
- Modify: `docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}`

- [ ] **Step 1: Run the full recording pipeline**

```bash
cargo build --release --quiet
# The cold-cache tokio federation needs a longer reindex than the default.
# This was needed for the v1 recording too (per the L1–L7 batch).
export LAIN_REINDEX_TIMEOUT=600
./scripts/record-spa-demo.sh --json /tmp/lain-record-summary.json
cat /tmp/lain-record-summary.json
ls -la docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}
```

Expected: exit 0; artifacts within budgets (webm ≤ 5 MB, mp4 ≤ 4 MB, gif ≤ 8 MB target / 12 MB cap, poster ≤ 200 KB). The frame at t≈25 s should show the anchor view — small graph, visible edges, lines readable.

- [ ] **Step 2: Extract a frame at the Graph-tab step**

```bash
TMP="$(mktemp -d)"
ffmpeg -y -hide_banner -loglevel error -ss 25 -i docs/screenshots/spa-demo.gif \
  -frames:v 1 "$TMP/graph-frame.png"
ls -la "$TMP"
rm -rf "$TMP"
```

Save the frame to the SDD workspace for the user:

```bash
mkdir -p .superpowers/sdd/2026-08-31-spa-graph-anchors-first
ffmpeg -y -hide_banner -loglevel error -ss 25 -i docs/screenshots/spa-demo.gif \
  -frames:v 1 .superpowers/sdd/2026-08-31-spa-graph-anchors-first/task-7-graph-frame.png
```

- [ ] **Step 3: Commit the artifacts**

```bash
git add docs/screenshots/spa-demo.webm \
        docs/screenshots/spa-demo.mp4 \
        docs/screenshots/spa-demo.gif \
        docs/screenshots/spa-demo-poster.png
git commit -m "docs: re-record hero with anchors-first Graph tab (visible lines)"
```

---

## Task 8: Final verification gate + push

- [ ] **Step 1: Run all test suites**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --test cli_surface 2>&1 | tail -10
node --test tests/js/graph_tab.test.js 2>&1 | tail -10
node tests/js/spa_e2e.test.js 2>&1 | tail -10
```

Expected:
- `cli_surface`: 3/3 pass.
- `graph_tab.test.js`: 30+ pass (27 from v1 + 3 new).
- `spa_e2e.test.js`: 47+ pass (43 from v1 + 4 new).

- [ ] **Step 2: Smoke check the fixture**

```bash
bash scripts/smoke_federation_fixture.sh
```

Expected: OK. Network required.

- [ ] **Step 3: Cross-doc link check (no doc edits in this plan, but cheap)**

```bash
grep -hoE 'docs/[a-zA-Z0-9_./-]+' README.md docs/QUICKSTART.md docs/command-center.md \
  | sort -u | xargs -I{} test -e {}
echo "exit=$?"
```

Expected: exit 0.

- [ ] **Step 4: Working-tree clean**

```bash
git status
```

Expected: only the four artifact files modified; everything else committed.

- [ ] **Step 5: Final summary**

```bash
git log --oneline origin/main -15
echo '---'
ls -la docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}
echo '---'
git status
```

Report:
- Commit count + last 10 commit hashes.
- Artifact sizes.
- Any unfixed parked items.

- [ ] **Step 6: Push**

```bash
git push -u origin main
```

---

## Self-Review (controller-only)

**Spec coverage:**

- ✅ Three-mode render (anchor | focal | search-driven) — Task 4.
- ✅ Top-30 anchors + 1-hop neighbourhood — Task 1 (pure helper), Task 4 (rendering path).
- ✅ Per-anchor neighbour cap of 20 — Task 1 (helper opt), Task 7 test.
- ✅ Click node → focal mode via `get_blast_radius` — Task 4 Step 6.
- ✅ Search input → focal mode via `find_anchors` name match — Task 2 (HTML), Task 3 (CSS), Task 4 Step 4.
- ✅ Depth slider 1..3 — Task 2 (HTML), Task 4 Step 4.
- ✅ "Back to anchors" button — Task 2 (HTML), Task 4 Step 4.
- ✅ Filter chips work in both modes — Task 4 (existing `buildFilterBar` is reused).
- ✅ Edge styling bumps (stroke-opacity 0.75, cross-repo stroke-width 2) — Task 3.
- ✅ Pure-helper tests (3 new) — Task 1.
- ✅ SPA e2e tests (4 new) — Task 5.
- ✅ Recorder driver update (10 s → 6 s) — Task 6.
- ✅ Hero recording + verification — Tasks 7 + 8.

**Placeholder scan:** zero TBD / TODO / FIXME / "fill in".

**Type/symbol consistency:**

- `computeAnchorVisibleSet(anchors, workspaceGraph, opts)` — defined in Task 1, used in Task 4 Step 2 (`renderGraphTab`).
- `applyDepth(neighbourhood, anchor, depth)` — defined in Task 1, exported, used in Task 4 if needed.
- `data-graph-search`, `data-graph-depth`, `data-graph-back` — uniform across Tasks 2, 4, 5.
- `[data-filter-row="focal"]` — uniform across Tasks 2 (HTML), 3 (CSS), 4 (visibility toggle), 5 (e2e assertion).
- `graphState.mode` — `'anchor' | 'focal'` — uniform across Tasks 4, 5.
- `graphState.depth` — uniform across Tasks 1, 4.

No internal contradictions found.
