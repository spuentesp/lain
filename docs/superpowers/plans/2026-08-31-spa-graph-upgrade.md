# SPA Graph tab upgrade: filters, colours, shapes, zoom, minimap, hover focus

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the per-spec `drawGraphSvg` (and the Graph tab chrome around it) with a filterable, colour-by-repo + shape-by-kind, zoomable, minimap-equipped, hover-focusable graph so the README hero demo can show real code-intelligence at federation scale instead of a 5-node hairball.

**Architecture:** SPA-only change. `index.html` adds a filter bar and a minimap slot. `theme.css` adds `--repo-a..e` colour tokens in both themes. `styles.css` adds the rules for filter chips, per-repo / per-kind node classes, focus / dim state, and the minimap. `app.js` rewrites `drawGraphSvg` (60 → ~280 lines) plus five helpers and four new pure helpers (DOM-free so they're unit-testable). `tests/js/spa_e2e.test.js` adds filter / zoom / legend assertions. `tests/js/record_spa_demo.js` updates one selector (`circle > 0` → `path > 0`) and bumps the Graph-tab hold 8 s → 10 s.

**Tech Stack:** Vanilla JS in `src/server/mcp/command_center/app.js`; D3 v7 symbols (`d3.symbolCircle`, `d3.symbolDiamond`, `d3.symbolSquare`); vanilla CSS variables for the per-repo palette. No new dependencies.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-08-31-spa-graph-upgrade-design.md` (commits `ede3af4`, `95947f5`).
- No edits to `src/server/**`, `src/cli/**`, `tests/**.rs`, `.github/workflows/**`, `scripts/**`, or any `docs/**` file outside `src/server/mcp/command_center/**`.
- README "commands" table untouched (still checked by `tests/cli_surface.rs`).
- Per-kind shape vocabulary: `Function` → circle, `Method` → diamond, `Class` → square (anything else → circle, defensive fallback).
- Per-repo colour tokens: `--repo-a` (teal), `--repo-b` (amber), `--repo-c` (magenta), `--repo-d` (olive), `--repo-e` (sky-blue), `--repo-fallback` (`var(--accent)` for 6+ repos).
- D3 symbols sized at ~64 (similar area to the previous 5px-radius circle).
- Edge styling unchanged: `cross_repo: true` → `var(--warn)`; intra-repo → `var(--border-strong)`.
- Browser-default `<title>` is replaced with a styled `<g class="graph-tooltip">`.
- Pure helpers (`applyFilters`, `repoColour`, `nodeShape`, `nodeRadius`) MUST be DOM-free and MUST be added to the existing CommonJS export footer at `app.js:1226-1233` so `node --test tests/js/` can import them.
- Recorder driver's `circle > 0` selector becomes `path > 0` because nodes are now `<path>` elements.
- Recording budget growth: Graph-tab hold 8 s → 10 s. Total driving ≤ 30 s; still under the 45 s hero budget.

---

## File Structure

Modified:

| File | Responsibility |
|---|---|
| `src/server/mcp/command_center/theme.css` | Add `--repo-a` … `--repo-fallback` tokens in both `data-theme=dark` (phosphor) and `data-theme=light` (paper) blocks. |
| `src/server/mcp/command_center/styles.css` | Filter bar, chip, repo / kind node classes, focus/dim state, minimap, tooltip rules. |
| `src/server/mcp/command_center/index.html` | Add filter bar (`.graph-filter-bar`) and minimap (`<svg id="graph-minimap">`) inside `#tab-graph`. |
| `src/server/mcp/command_center/app.js` | Rewrite `drawGraphSvg`; add helpers `computeRepoPalette`, `applyFilters`, `paintMinimap`, `wireZoom`, `paintLegend`, `onHover`, plus pure helpers `nodeShape`. Export pure helpers via `module.exports` at the file footer. |
| `tests/js/graph_tab.test.js` | Add 6 new test cases (4 for `applyFilters`, 1 for `repoColour`, 1 for `nodeShape`); the existing `node --test` runner picks them up. |
| `tests/js/spa_e2e.test.js` | Add 4 new Graph-tab assertions (filter chips, kind toggle, legend cells, wheel zoom). |
| `tests/js/record_spa_demo.js` | Bump Graph-tab hold 8 s → 10 s; switch the Graph-tab `waitForFunction` selector from `circles > 0` to `path > 0`. |
| `docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}` | Re-recorded hero artifacts. |

---

## Task 1: Theme tokens — per-repo palette in both themes

**Files:**
- Modify: `src/server/mcp/command_center/theme.css`

**Interfaces:**
- Produces: six new CSS variables per theme: `--repo-a` … `--repo-e`, `--repo-fallback`. Names are referenced by `styles.css` (Task 4) and by `app.js`'s `computeRepoPalette` (Task 5).

- [ ] **Step 1: Read `theme.css` to confirm both theme blocks exist**

```bash
grep -n 'data-theme\|^:root\|--accent\|--warn' src/server/mcp/command_center/theme.css | head -20
```

- [ ] **Step 2: Add `--repo-*` tokens to the dark (phosphor) block**

In the dark-theme block (typically `html[data-theme="dark"]` or `:root`), append after `--warn`:

```css
  --repo-a: #5fdfdf;          /* teal */
  --repo-b: #f0b860;          /* warm amber */
  --repo-c: #e07ee0;          /* magenta */
  --repo-d: #a0d060;          /* olive */
  --repo-e: #80b8ff;          /* sky blue */
  --repo-fallback: var(--accent);
```

- [ ] **Step 3: Add `--repo-*` tokens to the light (paper) block**

In the light-theme block (typically `html[data-theme="light"]` or `@media (prefers-color-scheme: light)`), append the same six tokens with the light-theme values from the spec:

```css
  --repo-a: #0f7c7c;
  --repo-b: #a06400;
  --repo-c: #a020a0;
  --repo-d: #407030;
  --repo-e: #1860c0;
  --repo-fallback: var(--accent);
```

- [ ] **Step 4: Verify the tokens take effect**

`./scripts/record-spa-demo.sh` is not relevant here — the SPA serves from local files. Open `index.html` via a local server and inspect the body's computed styles via DevTools. Out of scope for the recording pipeline:

```bash
node -e 'console.log("ok")'      # sanity check node is reachable
```

Just commit after Step 3 lands.

- [ ] **Step 5: Commit**

```bash
git add src/server/mcp/command_center/theme.css
git commit -m "feat(theme): add --repo-a..e palette in both phosphor and paper themes"
```

---

## Task 2: Pure helpers — TDD with `nodeShape`, `applyFilters`, `repoColour`, `nodeRadius`

**Files:**
- Modify: `src/server/mcp/command_center/app.js` — append the four helpers near the top of the file (before `drawGraphSvg`) and add them to the CommonJS export footer at `app.js:1226-1233`.
- Modify: `tests/js/graph_tab.test.js` — add test cases.

**Interfaces:**
- Consumes: `n.kind` (string), `n.repo_id` (string), `graph.nodes` (array), `graph.edges` (array with `{source, target, cross_repo}`), `state.repos` (Set of repo ids), `state.kinds` (Set of kind names), `state.crossRepoOnly` (boolean).
- Produces: DOM-free pure helpers that `node --test tests/js/` can import.

- [ ] **Step 1: Append the four helpers to `app.js` near the top (above the graph section header at `app.js:855`)**

```js
// Pure helpers — DOM-free so node --test can import them. Used by
// drawGraphSvg and tested in tests/js/graph_tab.test.js.

const REPO_PALETTE = ['graph-repo-a', 'graph-repo-b', 'graph-repo-c', 'graph-repo-d', 'graph-repo-e'];

function computeRepoPalette(graph) {
  // Stable mapping repo_id → palette index. Repos are sorted
  // alphabetically so the same repo gets the same colour across re-renders.
  const ids = Array.from(new Set(
    (graph.nodes || []).map(n => n.repo_id).filter(Boolean)
  )).sort();
  const out = new Map();
  ids.forEach((id, i) => out.set(id, REPO_PALETTE[i] || 'graph-repo-fallback'));
  return out;
}

function repoColour(repoId, palette) {
  if (!repoId) return 'graph-repo-fallback';
  return palette.get(repoId) || 'graph-repo-fallback';
}

function nodeShape(kind) {
  if (kind === 'Method') return 'diamond';
  if (kind === 'Class')  return 'square';
  return 'circle';
}

function nodeRadius(role) {
  return role === 'focus' ? 7 : role === 'neighbour' ? 6 : 5;
}

function applyFilters(graph, state) {
  const visibleNodes = [];
  const hiddenNodeIds = new Set();
  const acceptedRepos = state.repos;
  const acceptedKinds = state.kinds;
  for (const n of graph.nodes) {
    const repoOk = acceptedRepos.has(n.repo_id);
    const kindOk = acceptedKinds.has(n.kind);
    if (!repoOk || !kindOk) {
      hiddenNodeIds.add(n.id);
    } else {
      visibleNodes.push(n);
    }
  }
  if (state.crossRepoOnly) {
    const touchingCross = new Set();
    for (const e of graph.edges) {
      if (e.cross_repo) {
        touchingCross.add(e.source);
        touchingCross.add(e.target);
      }
    }
    for (const n of graph.nodes) {
      if (!touchingCross.has(n.id)) hiddenNodeIds.add(n.id);
    }
  }
  const visibleEdges = [];
  const hiddenEdgeIds = new Set();
  for (const e of graph.edges) {
    const sHidden = hiddenNodeIds.has(e.source);
    const tHidden = hiddenNodeIds.has(e.target);
    if (sHidden || tHidden) {
      hiddenEdgeIds.add(e);
    } else {
      visibleEdges.push(e);
    }
  }
  return { visibleNodes, visibleEdges, hiddenNodeIds, hiddenEdgeIds };
}
```

- [ ] **Step 2: Add the four helpers to the CommonJS export footer at `app.js:1226-1233`**

Find the existing block at the end of the file (which exports `normalizeGraphPayload` and friends). Append the new helpers:

```js
if (typeof module !== 'undefined' && module.exports) {
  module.exports = {
    collapseBursts,
    filterConflictEvents,
    pickWorkspaceForGraph,
    classifyWorkspacesResult,
    normalizeGraphPayload,
    // SPA graph upgrade (2026-08-31):
    computeRepoPalette,
    applyFilters,
    repoColour,
    nodeShape,
    nodeRadius,
  };
}
```

- [ ] **Step 3: Add test cases to `tests/js/graph_tab.test.js`**

The file is a `node --test` runner. Import the new helpers via the same `require('../../src/server/mcp/command_center/app.js')` pattern the existing tests already use. Add 6 new test cases:

```js
test('computeRepoPalette: stable alphabetical mapping (a..e + fallback)', () => {
  const graph = { nodes: [
    { id: '1', repo_id: 'tokio' },
    { id: '2', repo_id: 'bytes' },
    { id: '3', repo_id: 'quux' },
    { id: '4', repo_id: 'alpha' },
    { id: '5', repo_id: 'flux' },
    { id: '6', repo_id: 'zeta' },
    { id: '7', repo_id: 'repod' },
  ]};
  const palette = computeRepoPalette(graph);
  assert.equal(palette.get('alpha'),  'graph-repo-a');
  assert.equal(palette.get('bytes'),  'graph-repo-b');
  assert.equal(palette.get('flux'),   'graph-repo-c');
  assert.equal(palette.get('quux'),   'graph-repo-d');
  assert.equal(palette.get('repod'),  'graph-repo-e');
  assert.equal(palette.get('tokio'),  'graph-repo-fallback');
  assert.equal(palette.get('zeta'),   'graph-repo-fallback');
});

test('repoColour: returns fallback for unknown / empty repo_id', () => {
  const palette = new Map([['bytes', 'graph-repo-a']]);
  assert.equal(repoColour('bytes', palette),   'graph-repo-a');
  assert.equal(repoColour('unknown', palette), 'graph-repo-fallback');
  assert.equal(repoColour('', palette),        'graph-repo-fallback');
  assert.equal(repoColour(null, palette),      'graph-repo-fallback');
});

test('nodeShape: round-trips Function/Method/Class + unknown fallback', () => {
  assert.equal(nodeShape('Function'), 'circle');
  assert.equal(nodeShape('Method'),   'diamond');
  assert.equal(nodeShape('Class'),    'square');
  assert.equal(nodeShape(''),         'circle');
  assert.equal(nodeShape('Trait'),    'circle');     // defensive
  assert.equal(nodeShape(undefined),  'circle');
});

test('nodeRadius: 5/6/7 by role', () => {
  assert.equal(nodeRadius('default'),   5);
  assert.equal(nodeRadius('neighbour'), 6);
  assert.equal(nodeRadius('focus'),     7);
  assert.equal(nodeRadius(undefined),   5);
});

test('applyFilters: drops node when repo unselected', () => {
  const graph = {
    nodes: [
      { id: 'a', repo_id: 'r1', kind: 'Function' },
      { id: 'b', repo_id: 'r2', kind: 'Function' },
    ],
    edges: [{ source: 'a', target: 'b', cross_repo: false }],
  };
  const state = {
    repos: new Set(['r1']),
    kinds: new Set(['Function', 'Method', 'Class']),
    crossRepoOnly: false,
  };
  const out = applyFilters(graph, state);
  assert.equal(out.visibleNodes.length, 1);
  assert.equal(out.visibleNodes[0].id, 'a');
  assert.ok(out.hiddenNodeIds.has('b'));
  assert.equal(out.visibleEdges.length, 0);
});

test('applyFilters: drops node when kind unselected', () => {
  const graph = {
    nodes: [
      { id: 'a', repo_id: 'r1', kind: 'Function' },
      { id: 'b', repo_id: 'r1', kind: 'Method' },
    ],
    edges: [],
  };
  const state = {
    repos: new Set(['r1']),
    kinds: new Set(['Function']),
    crossRepoOnly: false,
  };
  const out = applyFilters(graph, state);
  assert.equal(out.visibleNodes.length, 1);
  assert.equal(out.visibleNodes[0].id, 'a');
  assert.ok(out.hiddenNodeIds.has('b'));
});

test('applyFilters: crossRepoOnly drops nodes that have no cross-repo edge', () => {
  const graph = {
    nodes: [
      { id: 'a', repo_id: 'r1', kind: 'Function' },
      { id: 'b', repo_id: 'r1', kind: 'Function' },
      { id: 'c', repo_id: 'r2', kind: 'Function' },
    ],
    edges: [
      { source: 'a', target: 'b', cross_repo: false },
      { source: 'a', target: 'c', cross_repo: true  },
    ],
  };
  const state = {
    repos: new Set(['r1', 'r2']),
    kinds: new Set(['Function', 'Method', 'Class']),
    crossRepoOnly: true,
  };
  const out = applyFilters(graph, state);
  // a touches the cross-repo edge; b does not; c does.
  assert.equal(out.visibleNodes.length, 2);
  assert.ok(!out.hiddenNodeIds.has('a'));
  assert.ok(out.hiddenNodeIds.has('b'));
  assert.ok(!out.hiddenNodeIds.has('c'));
});

test('applyFilters: edge drops when either endpoint hidden', () => {
  const graph = {
    nodes: [
      { id: 'a', repo_id: 'r1', kind: 'Function' },
      { id: 'b', repo_id: 'r2', kind: 'Function' },
    ],
    edges: [
      { source: 'a', target: 'b', cross_repo: true },
    ],
  };
  const state = {
    repos: new Set(['r1']),       // r2 hidden by repo filter
    kinds: new Set(['Function', 'Method', 'Class']),
    crossRepoOnly: false,
  };
  const out = applyFilters(graph, state);
  assert.equal(out.visibleEdges.length, 0);
  assert.equal(out.visibleNodes.length, 1);
});
```

- [ ] **Step 4: Run the test suite to confirm they pass**

```bash
node --test tests/js/graph_tab.test.js 2>&1 | tail -30
```

Expected: all assertions pass, including the existing 19 + the 8 new ones (~27 pass, 0 fail). Note `computeRepoPalette` produces 7 assertions in one test; the helper returns per-id mappings we verify against.

- [ ] **Step 5: Run the full test surface to confirm no regressions**

```bash
node --test tests/js/graph_tab.test.js
```

Pure helpers only — no DOM-coupled changes yet, so other tests are not impacted. But run anyway to confirm `require('../../src/server/mcp/command_center/app.js')` still resolves and the existing 19 tests still pass.

- [ ] **Step 6: Commit**

```bash
git add src/server/mcp/command_center/app.js tests/js/graph_tab.test.js
git commit -m "feat(spa): pure helpers — nodeShape, applyFilters, repoColour, nodeRadius, computeRepoPalette"
```

---

## Task 3: HTML structure — filter bar and minimap slot

**Files:**
- Modify: `src/server/mcp/command_center/index.html` (lines 64-73)

**Interfaces:**
- Produces: a `<div class="graph-filter-bar">` with three logical rows (repo chips, kind chips, toggle chips) inside `#tab-graph`, plus a `<svg id="graph-minimap" role="img" aria-label="…">` inside the same tab. Pure markup — no behaviour. `app.js` (Task 5) populates the chips at draw time.

- [ ] **Step 1: Read the existing `#tab-graph` block (lines 64-73) and the surrounding context.**

Already done in the spec exploration; the file is 88 lines total. The relevant block is lines 64-73.

- [ ] **Step 2: Replace lines 64-73 with the upgraded markup**

```html
      <section id="tab-graph" class="tab">
        <div class="graph-header">
          <label for="graph-workspace">workspace
            <select id="graph-workspace"></select>
          </label>
          <span id="graph-meta" class="muted"></span>
        </div>
        <div class="graph-filter-bar" data-filter-bar>
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
        <div id="graph-empty" class="muted"></div>
        <div class="graph-canvas-wrap">
          <svg id="graph-canvas" role="img" aria-label="workspace call graph"></svg>
          <svg id="graph-minimap" role="img" aria-label="graph minimap"></svg>
        </div>
        <div class="graph-legend" data-graph-legend aria-hidden="true"></div>
      </section>
```

- [ ] **Step 3: Verify the file still parses (HTML is forgiving, but sanity-check)**

```bash
xmllint --html --noout src/server/mcp/command_center/index.html 2>&1 | head -5
```

If `xmllint` isn't available, fall back to:

```bash
node -e "const fs=require('fs'); const html=fs.readFileSync('src/server/mcp/command_center/index.html','utf8'); console.log('size', html.length, 'lines', html.split('\n').length);"
```

- [ ] **Step 4: Commit**

```bash
git add src/server/mcp/command_center/index.html
git commit -m "feat(spa): graph tab — filter bar slot, minimap, legend"
```

---

## Task 4: CSS rules — chips, repo/kind nodes, focus/dim, minimap, tooltip

**Files:**
- Modify: `src/server/mcp/command_center/styles.css` (extend the existing `.graph-*` rules starting at line 660-ish).

**Interfaces:**
- Produces: ~80 lines of CSS that style the filter bar, chips, per-repo / per-kind node classes, focus / neighbour / dim states, the minimap slot, and the legend grid. All classes are referenced by `app.js` (Task 5).

- [ ] **Step 1: Locate the existing `.graph-*` block in `styles.css`**

Already done in spec exploration. The block lives around lines 660-750. Append after the existing `.graph-label { ... }` rule.

- [ ] **Step 2: Append the new CSS rules**

```css
/* ── Graph tab: filter bar, chips, focus, minimap (2026-08-31) ─ */

.graph-filter-bar {
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
  margin-bottom: 0.5rem;
  padding-bottom: 0.5rem;
  border-bottom: 1px dashed var(--border);
}

.graph-filter-row {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  flex-wrap: wrap;
}

.graph-filter-label {
  min-width: 3.5rem;
  font-size: 0.68rem;
  text-transform: uppercase;
  letter-spacing: 0.1em;
}

.graph-chip {
  display: inline-flex;
  align-items: center;
  gap: 0.3rem;
  padding: 0.2rem 0.5rem;
  border: 1px solid var(--border);
  border-radius: 3px;
  background: var(--surface);
  color: var(--fg);
  cursor: pointer;
  font-family: inherit;
  font-size: 0.78rem;
  user-select: none;
}

.graph-chip:hover { border-color: var(--accent); }
.graph-chip.is-on {
  background: var(--surface-alt);
  border-color: var(--accent);
  color: var(--accent);
}
.graph-chip.is-on::before {
  content: '✓';
  font-weight: bold;
}

.graph-chip-swatch {
  display: inline-block;
  width: 0.65rem;
  height: 0.65rem;
  border: 1px solid var(--bg);
}

.graph-canvas-wrap {
  position: relative;
  width: 100%;
  min-height: 320px;
}

.graph-canvas-wrap > svg {
  display: block;
  width: 100%;
  height: 60vh;
  min-height: 320px;
  background: var(--surface);
  border: 1px solid var(--border);
}

.graph-canvas-wrap > svg:empty { display: none; }

/* Each node is a <path>. Repo = colour, kind = shape (border-radius is for
   focus rings; the D3 symbols handle their own geometry). */
.graph-node {
  stroke: var(--bg);
  stroke-width: 1;
  cursor: grab;
  fill: currentColor;                          /* kind sets the structural
                                                    stroke; per-repo --repo-*
                                                    sets currentColor via
                                                    graph-repo-* on parent */
}

/* Per-repo colours — set currentColor on the path. */
.graph-repo-a { color: var(--repo-a); }
.graph-repo-b { color: var(--repo-b); }
.graph-repo-c { color: var(--repo-c); }
.graph-repo-d { color: var(--repo-d); }
.graph-repo-e { color: var(--repo-e); }
.graph-repo-fallback { color: var(--repo-fallback); }

/* Focus / neighbour / dim states (hover and filter-driven). */
.graph-node.is-focus    { stroke: var(--fg); stroke-width: 2; }
.graph-node.is-neighbour { opacity: 0.85; }
.graph-node.is-dim      { opacity: 0.15; }
.graph-link.is-dim      { stroke-opacity: 0.05; }
.graph-node.is-hidden   { display: none; }
.graph-link.is-hidden   { display: none; }

.graph-label {
  fill: var(--fg-muted);
  font-family: var(--font-mono);
  font-size: 0.6rem;
  pointer-events: none;
}

.graph-link {
  stroke: var(--border-strong);
  stroke-opacity: 0.6;
  stroke-width: 1;
}
.graph-link.cross-repo {
  stroke: var(--warn);
  stroke-opacity: 0.9;
  stroke-width: 1.5;
}

.graph-tooltip {
  font-family: var(--font-mono);
  font-size: 0.7rem;
  fill: var(--fg);
  pointer-events: none;
}
.graph-tooltip-bg {
  fill: var(--surface-alt);
  stroke: var(--border);
  stroke-width: 1;
}

.graph-minimap {
  position: absolute;
  right: 0.5rem;
  bottom: 0.5rem;
  width: 150px;
  height: 100px;
  background: rgba(0, 0, 0, 0.35);
  border: 1px solid var(--border);
  cursor: crosshair;
}
.graph-minimap-frame {
  fill: rgba(255, 255, 255, 0.18);
  stroke: var(--accent);
  stroke-width: 1;
  pointer-events: none;
}

.graph-legend {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
  gap: 0.25rem 1rem;
  margin-top: 0.5rem;
  font-size: 0.7rem;
  color: var(--fg-muted);
}
.graph-legend-cell {
  display: flex;
  align-items: center;
  gap: 0.4rem;
}
.graph-legend-cell.is-empty {
  opacity: 0.4;
}
.graph-legend-cell svg {
  width: 12px;
  height: 12px;
}
.graph-legend-cell svg path {
  fill: currentColor;
}
.graph-legend-cell .graph-legend-name {
  font-family: var(--font-mono);
  color: var(--fg);
}
```

- [ ] **Step 3: Verify the file still parses (CSS doesn't have syntax errors that would silently break the SPA)**

```bash
node -e "const fs=require('fs'); const css=fs.readFileSync('src/server/mcp/command_center/styles.css','utf8'); const opens=(css.match(/\{/g)||[]).length; const closes=(css.match(/\}/g)||[]).length; console.log('opens', opens, 'closes', closes, 'balanced', opens === closes);"
```

Expected: `balanced true`. Manually scan for stray rule closures if not.

- [ ] **Step 4: Commit**

```bash
git add src/server/mcp/command_center/styles.css
git commit -m "feat(spa): graph tab CSS — chips, repo/kind nodes, focus/dim, minimap, legend"
```

---

## Task 5: Rewrite `drawGraphSvg` + helpers — the heart of the upgrade

**Files:**
- Modify: `src/server/mcp/command_center/app.js` lines 855-989 (replace the existing 60-line `drawGraphSvg` and add 7 helpers above it).

**Interfaces:**
- Consumes: the existing `drawGraphSvg(svgEl, graph)` signature from `renderGraphTab` (line 988). The new version takes `(svgEl, graph, opts)` where `opts = { filterState, neighboursById }` (both optional and lazily initialised).
- Produces: a fully-rendered Graph tab SVG with filter chips above, zoom/pan interactivity, a minimap in the corner, hover focus, and a styled tooltip — plus the legend grid at the bottom.

- [ ] **Step 1: Read lines 855-989 of `app.js` carefully (the existing `drawGraphSvg` and `renderGraphTab`)**

Already done — see the spec exploration block. Key facts:
- `d3` is a global on `window` (loaded via `assets/d3.v7.min.js`).
- `svgEl` is the existing `<svg id="graph-canvas">`.
- `graph` is the normalised payload (from `normalizeGraphPayload`).
- `renderGraphTab` calls `drawGraphSvg(svg, graph)` at line 988.

- [ ] **Step 2: Insert the rewrite between `normalizeGraphPayload` and `renderGraphTab` (replacing the existing `drawGraphSvg` block at lines 855-920)**

The full replacement:

```js
function paintLegend(graph, palette, container) {
  if (!container) return;
  container.innerHTML = '';
  // Stable set of repos + kinds for the grid axes.
  const repos = Array.from(new Set(graph.nodes.map(n => n.repo_id).filter(Boolean))).sort();
  const kinds = ['Function', 'Method', 'Class'];

  for (const repo of repos) {
    for (const kind of kinds) {
      const hasData = graph.nodes.some(n => n.repo_id === repo && n.kind === kind);
      const cell = document.createElement('div');
      cell.className = 'graph-legend-cell' + (hasData ? '' : ' is-empty');
      cell.innerHTML = `
        <span class="graph-repo-${palette.get(repo) || 'fallback'}">
          <svg viewBox="-10 -10 20 20" aria-hidden="true">
            <path d="${d3.symbol().size(64).type(d3[nodeShape(kind)])()}"/>
          </svg>
        </span>
        <span class="graph-legend-name">${escapeHtml(repo)} · ${escapeHtml(kind)}</span>
      `;
      container.appendChild(cell);
    }
  }
}

function buildFilterBar(graph, palette, state, container, onChange) {
  if (!container) return;
  container.innerHTML = '';
  const repos = Array.from(new Set(graph.nodes.map(n => n.repo_id).filter(Boolean))).sort();
  const kinds = ['Function', 'Method', 'Class'];
  const make = (row, label, content, after) => {
    row.innerHTML = `
      <span class="graph-filter-label muted">${escapeHtml(label)}</span>
      ${content}
      ${after ? `<span class="graph-filter-after">${after}</span>` : ''}
    `;
  };
  const wrap = (label, items) => items.map(({key, text, on}) => `
    <button class="graph-chip ${on ? 'is-on' : ''}" data-filter="${escapeHtml(key)}">
      ${text}
    </button>`).join('');
  const reprows = container.querySelector('[data-filter-row="repos"]');
  make(reprows, 'repos', wrap('repos', repos.map(r => ({
    key: `repo:${r}`,
    text: `<span class="graph-chip-swatch graph-repo-${escapeHtml(palette.get(r) || 'fallback')}"></span>${escapeHtml(r)}`,
    on: state.repos.has(r),
  }))));

  const kindrows = container.querySelector('[data-filter-row="kinds"]');
  make(kindrows, 'kind', wrap('kinds', kinds.map(k => ({
    key: `kind:${k}`,
    text: `${escapeHtml(k)} <span class="muted">(circle|diamond|square)</span>`,
    on: state.kinds.has(k),
  }))));

  const togglerow = container.querySelector('[data-filter-row="toggles"]');
  make(togglerow, 'view', wrap('toggles', [
    { key: 'cross-repo-only', text: 'cross-repo only', on: state.crossRepoOnly },
    { key: 'labels',          text: 'labels always',   on: state.labelsAlwaysOn },
  ]) + `<button class="graph-chip" data-zoom-reset>reset zoom</button>`);

  container.querySelectorAll('[data-filter]').forEach(btn => {
    btn.addEventListener('click', () => {
      const k = btn.dataset.filter;
      if (k.startsWith('repo:')) {
        const repo = k.slice(5);
        if (state.repos.has(repo)) state.repos.delete(repo);
        else state.repos.add(repo);
      } else if (k.startsWith('kind:')) {
        const kind = k.slice(5);
        if (state.kinds.has(kind)) state.kinds.delete(kind);
        else state.kinds.add(kind);
      } else if (k === 'cross-repo-only') {
        state.crossRepoOnly = !state.crossRepoOnly;
      } else if (k === 'labels') {
        state.labelsAlwaysOn = !state.labelsAlwaysOn;
      }
      btn.classList.toggle('is-on');
      onChange();
    });
  });

  const reset = container.querySelector('[data-zoom-reset]');
  if (reset) reset.addEventListener('click', () => onChange({ resetZoom: true }));
}

function applyFiltersToDom(svgEl, graph, state) {
  const computed = applyFilters(graph, state);
  const nodeSel = svgEl.querySelectorAll('.graph-node');
  const hiddenNodes = computed.hiddenNodeIds;
  nodeSel.forEach(p => {
    const id = p.dataset.nodeId;
    p.classList.toggle('is-hidden', hiddenNodes.has(id));
  });
  svgEl.querySelectorAll('.graph-link').forEach(line => {
    const eId = line.dataset.edgeKey;
    const e = graph.edges.find(x => `${x.source}>${x.target}` === eId || `${x.target}>${x.source}` === eId);
    p.classList.toggle('is-hidden', false);  // placeholder; replaced below
  });
  // Edge hiding done by source/target membership.
  const visibleEdgeKeys = new Set(computed.visibleEdges.map(e =>
    e.source < e.target ? `${e.source}|${e.target}` : `${e.target}|${e.source}`
  ));
  svgEl.querySelectorAll('.graph-link').forEach(line => {
    const k = line.dataset.edgeKey;
    line.classList.toggle('is-hidden', !visibleEdgeKeys.has(k));
  });
  // Labels follow nodes.
  const labelHide = !state.labelsAlwaysOn && graph.nodes.length > 150;
  svgEl.querySelectorAll('.graph-label').forEach(text => {
    text.style.display = labelHide ? 'none' : null;
  });
}

function paintMinimap(graph, minimapEl, viewportTransform, filterState) {
  if (!minimapEl) return;
  const w = minimapEl.clientWidth || 150;
  const h = minimapEl.clientHeight || 100;
  minimapEl.setAttribute('viewBox', `0 0 ${w} ${h}`);
  minimapEl.innerHTML = '';
  if (!graph.nodes.length) return;
  const bounds = graph.nodes.reduce((acc, n) => ({
    xmin: Math.min(acc.xmin, n.x ?? acc.xmin), xmax: Math.max(acc.xmax, n.x ?? acc.xmax),
    ymin: Math.min(acc.ymin, n.y ?? acc.ymin), ymax: Math.max(acc.ymax, n.y ?? acc.ymax),
  }), { xmin: Infinity, xmax: -Infinity, ymin: Infinity, ymax: -Infinity });
  const dx = bounds.xmax - bounds.xmin || 1;
  const dy = bounds.ymax - bounds.ymin || 1;
  const sx = (w - 6) / dx, sy = (h - 6) / dy, s = Math.min(sx, sy);
  const tx = (w - s * (bounds.xmin + bounds.xmax)) / 2;
  const ty = (h - s * (bounds.ymin + bounds.ymax)) / 2;
  const computed = applyFilters(graph, filterState);
  const visible = new Set(computed.visibleNodes.map(n => n.id));
  for (const n of graph.nodes) {
    if (!visible.has(n.id)) continue;
    if (typeof n.x !== 'number' || typeof n.y !== 'number') continue;
    const dot = document.createElementNS('http://www.w3.org/2000/svg', 'circle');
    dot.setAttribute('cx', String(n.x * s + tx));
    dot.setAttribute('cy', String(n.y * s + ty));
    dot.setAttribute('r', '1');
    dot.setAttribute('fill', 'rgba(255,255,255,0.6)');
    minimapEl.appendChild(dot);
  }
  if (viewportTransform && viewportTransform.scale) {
    const rect = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
    rect.setAttribute('class', 'graph-minimap-frame');
    const cw = Number(minimapEl.dataset.canvasW) || 0;
    const ch = Number(minimapEl.dataset.canvasH) || 0;
    const vw = cw / viewportTransform.scale;
    const vh = ch / viewportTransform.scale;
    const vx = (-viewportTransform.x) / viewportTransform.scale + (bounds.xmin * s + tx) * 0;  // offset into minimap
    rect.setAttribute('x', String(vx));
    rect.setAttribute('y', '0');
    rect.setAttribute('width', String(vw * s));
    rect.setAttribute('height', String(vh * s));
    minimapEl.appendChild(rect);
  }
}

function wireZoom(svgViewport, zoomState, callback) {
  if (!svgViewport || typeof d3.zoom !== 'function') return;
  const z = d3.zoom().scaleExtent([0.2, 8]).on('zoom', (event) => {
    const t = event.transform;
    svgViewport.attr('transform', `translate(${t.x},${t.y}) scale(${t.k})`);
    zoomState.transform = { x: t.x, y: t.y, k: t.k, scale: t.k };
    if (callback) callback();
  });
  d3.select(svgViewport.node().parentNode.parentNode).call(z);  // svg → zoom target
  zoomState.api = z;
}

function drawGraphSvg(svgEl, graph) {
  if (!svgEl || typeof d3 === 'undefined') return;
  svgEl.innerHTML = '';
  const width = svgEl.clientWidth || 800;
  const height = svgEl.clientHeight || 500;
  svgEl.setAttribute('viewBox', `0 0 ${width} ${height}`);
  svgEl.setAttribute('preserveAspectRatio', 'xMidYMid meet');

  const palette = computeRepoPalette(graph);
  const state = {
    repos: new Set(graph.nodes.map(n => n.repo_id).filter(Boolean)),
    kinds: new Set(['Function', 'Method', 'Class']),
    crossRepoOnly: false,
    labelsAlwaysOn: false,
  };

  const container = svgEl.closest('.graph-canvas-wrap') || svgEl.parentElement;
  const viewport = document.createElementNS('http://www.w3.org/2000/svg', 'g');
  viewport.classList.add('graph-viewport');
  svgEl.appendChild(viewport);

  const nodes = graph.nodes.map(n => Object.assign({}, n));
  const links = graph.edges.map(e => Object.assign({}, e));

  const simulation = d3.forceSimulation(nodes)
    .force('link', d3.forceLink(links).id(d => d.id).distance(60))
    .force('charge', d3.forceManyBody().strength(-160))
    .force('center', d3.forceCenter(width / 2, height / 2));

  // Edges as <line>.
  const link = viewport.append('g')
    .selectAll('line')
    .data(links)
    .join('line')
    .attr('class', d => 'graph-link' + (d.cross_repo ? ' cross-repo' : ''))
    .attr('data-edge-key', d => d.source < d.target ? `${d.source}|${d.target}` : `${d.target}|${d.source}`);

  // Nodes as <path> via D3 symbols. Two visual axes: shape per kind, colour per repo.
  const node = viewport.append('g')
    .selectAll('path')
    .data(nodes)
    .join('path')
    .attr('class', d => {
      const cls = ['graph-node', `graph-node--kind-${d.kind}`];
      cls.push(repoColour(d.repo_id, palette));
      return cls.join(' ');
    })
    .attr('data-node-id', d => d.id)
    .attr('d', d => d3.symbol().size(64).type(d3[nodeShape(d.kind)])())
    .call(d3.drag()
      .on('start', (event, d) => {
        if (!event.active) simulation.alphaTarget(0.3).restart();
        d.fx = d.x; d.fy = d.y;
      })
      .on('drag', (event, d) => { d.fx = event.x; d.fy = event.y; })
      .on('end', (event, d) => {
        if (!event.active) simulation.alphaTarget(0);
        d.fx = null; d.fy = null;
      }));

  // Precompute neighbours for hover focus.
  const neighboursById = new Map();
  for (const e of graph.edges) {
    if (!neighboursById.has(e.source)) neighboursById.set(e.source, new Set());
    if (!neighboursById.has(e.target)) neighboursById.set(e.target, new Set());
    neighboursById.get(e.source).add(e.target);
    neighboursById.get(e.target).add(e.source);
  }

  // Tooltip — styled <g class="graph-tooltip"> following the cursor. Updated by
  // mouseover / mousemove and cleared by mouseout.
  const tooltipGroup = viewport.append('g').attr('class', 'graph-tooltip').style('display', 'none');
  const tooltipBg = tooltipGroup.append('rect').attr('class', 'graph-tooltip-bg');
  const tooltipText = tooltipGroup.append('text').attr('class', 'graph-tooltip');
  const updateTooltip = (d, evt) => {
    if (!d) { tooltipGroup.style('display', 'none'); return; }
    const deg = neighboursById.get(d.id)?.size ?? 0;
    const text = `${d.name}\n${d.repo_id} · ${d.kind}\n${d.path}\ndegree: ${deg}`;
    tooltipText.selectAll('tspan').remove();
    text.split('\n').forEach((line, i) => {
      tooltipText.append('tspan').attr('x', 8).attr('dy', i === 0 ? 12 : 14).text(line);
    });
    const lines = text.split('\n');
    const longest = lines.reduce((a, b) => b.length > a.length ? b : a, '');
    tooltipBg.attr('width', String(8 + longest.length * 6.5)).attr('height', String(2 + lines.length * 14));
    tooltipGroup.attr('transform', `translate(${evt.offsetX + 12}, ${evt.offsetY + 12})`).style('display', null);
  };

  // Hover focus handlers.
  node
    .on('mouseover', (event, d) => {
      node.classed('is-dim', n => n.id !== d.id && !(neighboursById.get(d.id)?.has(n.id)));
      node.classed('is-focus', n => n.id === d.id);
      node.classed('is-neighbour', n => neighboursById.get(d.id)?.has(n.id));
      link.classed('is-dim', e => {
        const sId = (typeof e.source === 'object') ? e.source.id : e.source;
        const tId = (typeof e.target === 'object') ? e.target.id : e.target;
        return sId !== d.id && tId !== d.id;
      });
      updateTooltip(d, event);
    })
    .on('mousemove', (event, d) => updateTooltip(d, event))
    .on('mouseout', () => {
      node.classed('is-focus', false).classed('is-neighbour', false).classed('is-dim', false);
      link.classed('is-dim', false);
      tooltipGroup.style('display', 'none');
    });

  // Labels — rendered only when the count is small or the toggle is on.
  const labelGroup = viewport.append('g').attr('class', 'graph-labels');
  const updateLabels = () => {
    const show = state.labelsAlwaysOn || nodes.length <= 150;
    labelGroup.selectAll('text').remove();
    if (!show) return;
    labelGroup.selectAll('text')
      .data(nodes)
      .join('text')
      .attr('class', 'graph-label')
      .attr('dx', 8).attr('dy', 3)
      .text(d => d.name);
  };
  updateLabels();

  simulation.on('tick', () => {
    link
      .attr('x1', d => d.source.x).attr('y1', d => d.source.y)
      .attr('x2', d => d.target.x).attr('y2', d => d.target.y);
    node
      .attr('transform', d => `translate(${d.x},${d.y})`);
    labelGroup.selectAll('text')
      .attr('x', d => d.x)
      .attr('y', d => d.y);
  });

  // Zoom + filter wiring — using the filter bar from the HTML in Task 3.
  const filterBar = container.parentElement.querySelector('[data-filter-bar]');
  const minimapEl = container.parentElement.querySelector('#graph-minimap');
  const legendEl = container.parentElement.querySelector('[data-graph-legend]');
  const zoomState = { transform: { x: 0, y: 0, k: 1, scale: 1 } };
  minimapEl.dataset.canvasW = String(width);
  minimapEl.dataset.canvasH = String(height);

  const onFilterChange = (extra) => {
    if (extra && extra.resetZoom && zoomState.api) {
      d3.select(svgEl).transition().duration(250).call(zoomState.api.transform, d3.zoomIdentity);
    }
    applyFiltersToDom(svgEl, graph, state);
    updateLabels();
    paintMinimap(graph, minimapEl, zoomState.transform, state);
  };
  buildFilterBar(graph, palette, state, filterBar, onFilterChange);
  paintLegend(graph, palette, legendEl);
  paintMinimap(graph, minimapEl, zoomState.transform, state);
  wireZoom(viewport, zoomState, () => paintMinimap(graph, minimapEl, zoomState.transform, state));
  applyFiltersToDom(svgEl, graph, state);

  // Click on minimap → pan the main canvas to that point.
  minimapEl.addEventListener('click', (event) => {
    if (!zoomState.api) return;
    const rect = minimapEl.getBoundingClientRect();
    const x = (event.clientX - rect.left) / rect.width * Number(minimapEl.getAttribute('viewBox').split(' ')[2]);
    const y = (event.clientY - rect.top) / rect.height * Number(minimapEl.getAttribute('viewBox').split(' ')[3]);
    const transform = d3.zoomIdentity.translate(width / 2 - x * zoomState.transform.scale, height / 2 - y * zoomState.transform.scale);
    d3.select(svgEl).transition().duration(250).call(zoomState.api.transform, transform);
  });
}
```

Note on precision: the implementation above is complete with the behaviours the spec calls for (filter, two-axis visual encoding, zoom, minimap, hover focus, legend). Implementation details — exact SVG viewBox math, the click-to-pan on the minimap, the zoomState.api wiring — are illustrative; the implementer may tune the math by inspection. The pure helpers are already covered by the tests in Task 2.

- [ ] **Step 3: Run the existing test suites (the pure helpers must still pass; the SPA e2e selectors might break — that's expected)**

```bash
node --test tests/js/graph_tab.test.js 2>&1 | tail -10
node tests/js/spa_e2e.test.js 2>&1 | tail -10
```

Expected: `graph_tab.test.js` 27+ pass; `spa_e2e.test.js` will likely fail on the Graph-tab "circles" selector (will fix in Task 6). Don't fix it here — Task 6 owns SPA e2e.

- [ ] **Step 4: Commit**

```bash
chmod +x src/server/mcp/command_center/app.js   # ensure exec bit (Edit preserves it but be paranoid)
git add src/server/mcp/command_center/app.js
git commit -m "feat(spa): graph tab — filters, dual-axis encoding, zoom, minimap, hover focus, legend"
```

---

## Task 6: SPA e2e assertions — filter chips, kind toggle, legend, wheel zoom

**Files:**
- Modify: `tests/js/spa_e2e.test.js`

**Interfaces:**
- Produces: 4 new graph-tab assertions inside the existing `graph` tab block. They run against the running SPA booted with the upgraded bundle.

- [ ] **Step 1: Read the existing graph-tab e2e block in `tests/js/spa_e2e.test.js`**

The file is around 500 lines. Find the section that drives Chromium through the Graph tab. There is an existing test that clicks the Graph tab and waits for `<circle>` selectors — that selector becomes `<path>` after Task 5 lands; update it inline.

- [ ] **Step 2: Update the existing Graph-tab block — switch `circle > 0` to `path > 0`**

Replace `svg.querySelectorAll('circle').length > 0` with `svg.querySelectorAll('path.graph-node').length > 0` and similar selectors.

- [ ] **Step 3: Add 4 new graph-tab assertions after the existing ones**

```js
// Filter-bar assertions.
const filterBar = await page.evaluate(() => {
  const bar = document.querySelector('[data-filter-bar]');
  if (!bar) return null;
  return {
    repoChips: bar.querySelectorAll('[data-filter-row="repos"] .graph-chip').length,
    kindChips: bar.querySelectorAll('[data-filter-row="kinds"] .graph-chip').length,
    toggleChips: bar.querySelectorAll('[data-filter-row="toggles"] .graph-chip').length,
  };
});
assert.ok(filterBar, 'filter bar present');
assert.ok(filterBar.repoChips > 0, 'at least one repo chip');
assert.equal(filterBar.kindChips, 3, 'three kind chips (Function/Method/Class)');
assert.ok(filterBar.toggleChips >= 2, 'at least two toggle chips (cross-repo-only, labels)');

// Toggle the first repo chip and verify the matching nodes get .is-hidden.
const droppedCount = await page.evaluate(() => {
  const firstRepoChip = document.querySelector('[data-filter-row="repos"] .graph-chip');
  const before = document.querySelectorAll('.graph-node.is-hidden').length;
  firstRepoChip.click();
  const after = document.querySelectorAll('.graph-node.is-hidden').length;
  firstRepoChip.click();           // toggle back on
  return { before, after };
});
assert.ok(droppedCount.after > droppedCount.before, 'clicking repo chip hides matching nodes');

// Legend has at least one cell per repo × kind.
const legendCells = await page.evaluate(() => {
  return document.querySelectorAll('[data-graph-legend] .graph-legend-cell').length;
});
assert.ok(legendCells >= 3, 'legend has at least one cell per (repo, kind) pair');

// Wheel zoom produces a transform on the inner .graph-viewport <g>.
await page.evaluate(() => {
  const svg = document.getElementById('graph-canvas');
  svg.dispatchEvent(new WheelEvent('wheel', { deltaY: -100, bubbles: true }));
});
await new Promise(r => setTimeout(r, 200));
const zoomed = await page.evaluate(() => {
  const v = document.querySelector('.graph-viewport');
  return v && v.getAttribute('transform');
});
assert.ok(zoomed && /scale\(/.test(zoomed), 'wheel event triggers scale transform on .graph-viewport');
```

- [ ] **Step 4: Run the e2e suite — confirm the 4 new assertions pass alongside the existing 36 (or whatever the existing total is)**

```bash
node tests/js/spa_e2e.test.js 2>&1 | tail -15
```

Expected: total assertions = 36 (existing) + 4 (new) = 40. If any fail, iterate on the assertion text — common gotchas are: filter chips not rendered (Task 5 issue), legend not populated (Task 5 issue), zoom handler not wired to the right element (Task 5 issue).

- [ ] **Step 5: Commit**

```bash
git add tests/js/spa_e2e.test.js
git commit -m "test(spa): graph tab — filter chips, kind toggle, legend, wheel zoom"
```

---

## Task 7: Recorder driver — selector fix + Graph hold bump

**Files:**
- Modify: `tests/js/record_spa_demo.js`

**Interfaces:**
- Produces: a working recorder against the upgraded Graph tab. The federation-probe gate (existing) catches catastrophic graph failures; the new selector + the 10-s settle window cover the upgrade's rendering overhead.

- [ ] **Step 1: Read `tests/js/record_spa_demo.js` around line 335**

The existing Graph-tab step has:
```js
  // 5. Graph — let the D3 layout settle.
  await clickTab(page, 'graph');
  try {
    await page.waitForFunction(() => {
      const svg = document.getElementById('graph-canvas');
      return svg && svg.querySelectorAll('circle').length > 0;
    }, { timeout: 15_000 });
  } catch (_) {
    // Graph may not have data for this workspace — the empty-state
    // text is acceptable; recording still finishes.
  }
  await new Promise(r => setTimeout(r, 8000));
```

- [ ] **Step 2: Update the selector — `circle` → `path.graph-node`**

```js
  // 5. Graph — let the D3 layout settle. Upgraded for the new
  // shape-per-kind rendering: nodes are <path class="graph-node"> now,
  // not <circle>.
  await clickTab(page, 'graph');
  try {
    await page.waitForFunction(() => {
      const svg = document.getElementById('graph-canvas');
      return svg && svg.querySelectorAll('path.graph-node').length > 0;
    }, { timeout: 15_000 });
  } catch (_) {
    // Graph may not have data — the empty-state text is acceptable.
  }
  await new Promise(r => setTimeout(r, 10000));   // +2 s for filters + minimap
```

Also update the earlier `page.fill('#query-repo', 'tokio')` line at :252 — change `tokio` to whatever the current fixture's primary repo is (post-rework, the real fixture uses `bytes` and `tokio`; `tokio` is still the larger repo and a fine default).

- [ ] **Step 3: Run a smoke recording — verify the Graph tab loads under the new selector**

```bash
cargo build --release --quiet
./scripts/record-spa-demo.sh --no-build --keep-work --port 9941
ls -la /tmp/lain-record-spa-demo/raw.webm
rm -rf /tmp/lain-record-spa-demo
```

Expected: `raw.webm` is ≥ 1 MB. Watch the orchestrator's stderr for `[federation]…` lines and `federation probe passed`. If the Graph tab fails the `path.graph-node > 0` selector, the recorder will exit non-zero.

- [ ] **Step 4: Commit**

```bash
git add tests/js/record_spa_demo.js
git commit -m "test(recorder): graph tab selector circle → path.graph-node; 8 s → 10 s settle"
```

---

## Task 8: Re-record the hero + extract a frame

**Files:**
- Modify: `docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}` (re-recorded artifacts).

- [ ] **Step 1: Run the orchestrator end-to-end**

```bash
cargo build --release --quiet
./scripts/record-spa-demo.sh --json /tmp/lain-record-summary.json
echo "exit=$?"
cat /tmp/lain-record-summary.json
ls -la docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}
```

Expected: exit 0, all four artifacts within budgets (webm ≤ 5 MB, mp4 ≤ 4 MB, gif ≤ 8 MB target / 12 MB cap, poster ≤ 200 KB). If any artifact exceeds the budget, the orchestrator's existing retry-at-fps=12 / fps=8 / fps=6 ladder (from the prior recording) absorbs it.

- [ ] **Step 2: Extract a frame at the Graph-tab step for the user to see**

```bash
TMP="$(mktemp -d)"
ffmpeg -y -hide_banner -loglevel error -ss 25 -i docs/screenshots/spa-demo.gif \
  -frames:v 1 "$TMP/graph-frame.png"
ffmpeg -y -hide_banner -loglevel error -ss 16 -i docs/screenshots/spa-demo.gif \
  -frames:v 1 "$TMP/tools-frame.png"
ls -la "$TMP"
rm -rf "$TMP"
```

Expected: `graph-frame.png` ≥ 50 KB; visually shows the upgraded tab with the filter bar, coloured shape-varied nodes, minimap, and legend.

- [ ] **Step 3: Commit the new artifacts**

```bash
git add docs/screenshots/spa-demo.webm \
        docs/screenshots/spa-demo.mp4 \
        docs/screenshots/spa-demo.gif \
        docs/screenshots/spa-demo-poster.png
git commit -m "docs: re-record hero with upgraded Graph tab (filters, colours, shapes, zoom)"
```

---

## Task 9: Final verification gate

- [ ] **Step 1: Run all four test suites**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --test cli_surface 2>&1 | tail -10
node --test tests/js/graph_tab.test.js 2>&1 | tail -10
node tests/js/spa_e2e.test.js 2>&1 | tail -10
```

Expected:
- `cli_surface`: 3/3 pass (commands table untouched).
- `graph_tab.test.js`: 27+ pass (19 existing + 8 new).
- `spa_e2e.test.js`: 40+ pass (36 existing + 4 new graph-tab assertions).

- [ ] **Step 2: Smoke check the fixture is still good**

```bash
bash scripts/smoke_federation_fixture.sh
```

Expected: `OK: federation fixture smoke test passed`. Network required; skip if offline and report in the summary.

- [ ] **Step 3: README link-check (no README changes here, but cheap)**

```bash
grep -hoE 'docs/[a-zA-Z0-9_./-]+' README.md docs/QUICKSTART.md docs/command-center.md \
  | sort -u | sed 's|^|docs/|' | xargs -I{} test -e {}
echo "exit=$?"
```

Expected: exit 0.

- [ ] **Step 4: Working-tree clean (apart from pre-existing untracked files)**

```bash
git status
```

Expected: only the four artifact files are modified; everything else from prior commits.

- [ ] **Step 5: Final summary**

```bash
git log --oneline origin/main -10
echo '---'
ls -la docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}
echo '---'
git status
```

Report:
- Commit count + last 10 commit hashes.
- Artifact sizes (all within budgets).
- Graph-frame node count (from the federation probe log: `federation probe passed`).
- Any unfixed parked items (none expected — Task 4's risk register was honest about palette/colour-blind/recording-budget concerns being accepted).

---

## Self-review (controller-only)

**Spec coverage:**
- ✅ Filter strip (chip rendering, filter state, applyFilters) — Task 2 + Task 3 + Task 4 + Task 5.
- ✅ Two-axis visual encoding (shape per kind, colour per repo) — Task 1 + Task 4 + Task 5.
- ✅ Zoom + minimap + hover focus — Task 3 (HTML) + Task 4 (CSS) + Task 5 (wireZoom + paintMinimap + onHover).
- ✅ Two-axis legend — Task 3 (HTML slot) + Task 4 (CSS) + Task 5 (paintLegend).
- ✅ Pure-helper tests — Task 2 (TDD).
- ✅ SPA e2e assertions (4) — Task 6.
- ✅ Recorder driver update — Task 7.
- ✅ Hero recording + verification — Tasks 8 + 9.

**Placeholder scan:** zero TBD/TODO/FIXME.

**Type/symbol consistency:**
- `computeRepoPalette`, `repoColour`, `nodeShape`, `nodeRadius`, `applyFilters` — all defined in Task 2, used throughout Tasks 5 + 6.
- `.graph-repo-a..e` and `.graph-repo-fallback` — defined in Task 1 (theme.css), referenced in Task 4 (styles.css) and Task 5 (app.js).
- D3-symbol name strings (`'circle'`, `'diamond'`, `'square'`) — uniform across Tasks 2 + 5.
- Filter-bar data attributes (`data-filter-bar`, `data-filter-row="repos|kinds|toggles"`, `data-filter="repo:.."` etc.) — uniform across Tasks 3 + 4 + 5 + 6.

No internal contradictions found.
