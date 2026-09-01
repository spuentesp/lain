# SPA Graph tab: live-MCP fixes (Items 13 + 14)

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development (recommended) or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix two live-MCP-path bugs in the SPA's Graph tab so anchor mode and focal mode render real nodes/edges against the `bytes + tokio` federation. Item 14 — `find_anchors` needs `repo_id`. Item 13 — focal mode switches from text-returning `get_blast_radius` to JSON-returning `get_cross_repo_blast_radius_for_repo` (which also accepts `depth: "1..3"` so the depth slider actually works).

**Architecture:** SPA-only change to `src/server/mcp/command_center/{app.js, tests/js/...}`. Add one pure helper `applyFocalGraph(payload)` to `app.js` (TDD), wire it into the focal branch of `renderGraphTab`, pass `repo_id` from the active workspace's first repo into the anchor branch's `find_anchors` call, switch the focal call to `get_cross_repo_blast_radius_for_repo`. After the SPA edit, `cargo build --release` so the recorder / e2e see the change (Rust binary `include_bytes!`'s `app.js`). Re-record the hero artifacts, then push.

**Tech Stack:** Vanilla JS in `app.js`; D3 v7 rendering is unchanged. No new dependencies.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-09-01-spa-graph-mcp-text-fixes-design.md` (commit `a4727da`).
- Plan supersedes nothing — no edits to README, scripts, Rust source, .github, or docs outside `src/server/mcp/command_center/**` and `tests/js/**`.
- `app.js` is `include_bytes!`'d at `src/server/mcp/command_center_assets.rs:31`. After ANY edit to `app.js`, run `cargo build --release --quiet` before the recorder / e2e can see it.
- `~/.cargo/bin/cargo` is a broken symlink — use `export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"` for any cargo invocation.
- Cold-cache federation indexing overruns the 60 s `LAIN_INDEX_TIMEOUT` default. Recording requires `LAIN_REINDEX_TIMEOUT=600` + the recorder-driver `waitForReady` cap bumped to 600 s (already done in the prior plan's Task 7).
- Pure helpers (`applyFocalGraph`) DOM-free, exported via the CommonJS export footer at the end of `app.js`.
- Symbol for the recorder's focal step stays `data_mut` (unambiguous cross-repo symbol chosen in the prior plan).

---

## File Structure

Modified:

| File | Responsibility |
|---|---|
| `src/server/mcp/command_center/app.js` | Add `applyFocalGraph` pure helper. Update `renderGraphTab` — anchor branch passes `repo_id` from the workspace graph; focal branch switches to `get_cross_repo_blast_radius_for_repo`, parses JSON, uses `applyFocalGraph`. Add `graphState.focalRepoId` field. Update the click handler in `drawGraphSvg` and the search-input handler in `wireGraphControls` to set it. |
| `tests/js/graph_tab.test.js` | Append 3 new pure-helper tests (1 for `applyFocalGraph` × 2 shapes; 1 for `computeAnchorVisibleSet` repo-pinning). |
| `tests/js/spa_e2e.test.js` | Append 1 new Graph-tab assertion verifying focal mode renders `.graph-node > 0` after the search input submits. |
| `docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}` | Re-recorded hero artifacts — the focal frame at t≈35 s will now actually show nodes and edges (after Item 13's fix). |

Created: none.

Untouched (explicitly):

- `src/server/**` (handlers are correct; the SPA was wrong).
- `tests/js/record_spa_demo.js` (already correct — uses `data_mut` from the prior fix wave).
- `scripts/**`, `.github/workflows/**`, `Cargo.*`, README, `docs/**` files outside `src/server/mcp/command_center/**` and `tests/js/**`.

---

## Task 1: Pure helper — `applyFocalGraph` (TDD)

**Files:**
- Modify: `src/server/mcp/command_center/app.js` — add the helper near the existing pure helpers (`computeAnchorVisibleSet`/`applyFilters`/`computeRepoPalette` block around `app.js:860-1010`) and add it to the CommonJS export footer at the file's bottom.
- Modify: `tests/js/graph_tab.test.js` — append 2 tests for `applyFocalGraph` + 1 test for `computeAnchorVisibleSet` repo-pinning.

**Interfaces:**
- Consumes: `payload.by_repo` (`BTreeMap<String, Vec<{name, repo_id, kind, path}>>`), `payload.total_count: number`, `payload.truncated: boolean`, `focalSymbol: string` (the symbol the user clicked / typed).
- Produces: `{ nodes: Node[], edges: Edge[] }` ready for the existing `drawGraphSvg` (which already calls `normalizeGraphPayload`; here we build the same shape). Edges have `cross_repo: true` if source and target have different `repo_id`.

- [ ] **Step 1: Append the helper to `app.js` immediately after `computeAnchorVisibleSet`'s closing brace (around line 1010, depending on prior edits)**

```js
// Build a visible-set from a `get_cross_repo_blast_radius_for_repo` JSON
// payload. Pure helper used by renderGraphTab's focal branch.
//
// payload shape:
//   { by_repo: { [repoId]: [{ name, repo_id, kind, path }, ...] },
//     total_count: number,
//     truncated: boolean }
// focalSymbol — the symbol the user clicked or typed; used as the
// visual centre of the focal graph (highlighted via .is-focus).
function applyFocalGraph(payload, focalSymbol) {
  const byRepo = (payload && payload.by_repo) || {};
  const nodes = [];
  const edges = [];
  let focalPushed = false;
  // Look up the focal symbol's repo by matching the symbol name against
  // byRepo[*]; if found, the focal goes once into that repo's bucket and
  // we mark it as the centre. If the symbol isn't in any bucket (rare —
  // happens when the user-typed name is not a known dependency), we
  // synthesise a minimal focal node so the canvas isn't empty.
  let focalRepo = null;
  for (const repoId of Object.keys(byRepo)) {
    const items = byRepo[repoId] || [];
    for (const n of items) {
      if (n && n.name === focalSymbol) { focalRepo = repoId; break; }
    }
    if (focalRepo) break;
  }
  for (const repoId of Object.keys(byRepo)) {
    const items = byRepo[repoId] || [];
    for (const n of items) {
      if (!n || !n.name) continue;
      nodes.push({
        id: `${repoId}::${n.name}`,
        name: n.name,
        path: n.path || '',
        repo_id: repoId,
        kind: n.kind || 'Function',
      });
      // Connect every entry to the focal symbol as a star.
      if (n.name === focalSymbol) {
        focalPushed = true;
        continue;
      }
      edges.push({
        source: `${repoId}::${focalSymbol}`,
        target: `${repoId}::${n.name}`,
        edge_type: 'Calls',
        cross_repo: false,
      });
      // And connect across repos when the focal lives in a different
      // repo (focal symbol might not appear in *every* repo, only in the
      // one it was defined in — but the caller's neighbours cross the
      // boundary via the focal).
      if (focalRepo && focalRepo !== repoId) {
        edges.push({
          source: `${focalRepo}::${focalSymbol}`,
          target: `${repoId}::${n.name}`,
          edge_type: 'Calls',
          cross_repo: true,
        });
      }
    }
  }
  if (!focalPushed) {
    // Fallback: synthesise a focal node so the canvas isn't blank.
    nodes.push({
      id: `${focalRepo || 'unknown'}::${focalSymbol}`,
      name: focalSymbol,
      path: '',
      repo_id: focalRepo || 'unknown',
      kind: 'Function',
    });
  }
  return { nodes, edges, truncated: !!(payload && payload.truncated) };
}
```

- [ ] **Step 2: Add `applyFocalGraph` to the CommonJS export footer**

Find the export footer at the bottom of `app.js` (post-Task 4 round 3 it contains both v1 and v2 exports — find by `grep -n 'applyDepth' src/server/mcp/command_center/app.js`) and append:

```js
// In the module.exports object literal:
applyFocalGraph,
```

- [ ] **Step 3: Append 3 new test cases to `tests/js/graph_tab.test.js`**

```js
test('applyFocalGraph: builds visible set from by_repo JSON with cross-repo edges', () => {
  const payload = {
    by_repo: {
      bytes: [
        { name: 'data_mut', repo_id: 'bytes', kind: 'Function', path: 'src/foo.rs' },
        { name: 'Bytes',    repo_id: 'bytes', kind: 'Class',    path: 'src/bar.rs' },
      ],
      tokio: [
        { name: 'tokio',    repo_id: 'tokio', kind: 'Function', path: 'src/lib.rs' },
      ],
    },
    total_count: 3,
    truncated: false,
  };
  const out = applyFocalGraph(payload, 'data_mut');
  // 3 visible nodes: data_mut (centre), Bytes, tokio.
  assert.equal(out.nodes.length, 3);
  const ids = new Set(out.nodes.map(n => n.id));
  assert.ok(ids.has('bytes::data_mut'));
  assert.ok(ids.has('bytes::Bytes'));
  assert.ok(ids.has('tokio::tokio'));
  // 2 edges: bytes::Bytes ↔ data_mut (intra), tokio::tokio ↔ data_mut (cross).
  assert.equal(out.edges.length, 2);
  const crossCount = out.edges.filter(e => e.cross_repo).length;
  assert.equal(crossCount, 1);
});

test('applyFocalGraph: synthesises focal node when focal not in by_repo', () => {
  const payload = {
    by_repo: {
      bytes: [{ name: 'Bytes', repo_id: 'bytes', kind: 'Class', path: 'src/bytes.rs' }],
    },
  };
  const out = applyFocalGraph(payload, 'Unknown');
  // 2 nodes: the unknown focal (synthesised) + the Bytes class.
  assert.equal(out.nodes.length, 2);
  const synth = out.nodes.find(n => n.name === 'Unknown');
  assert.ok(synth, 'synthesised focal node present');
  // truncated flag reflects payload.
  assert.equal(out.truncated, false);
});

test('computeAnchorVisibleSet: explicit repo_id matches only that repo (no cross-repo bleed)', () => {
  const anchors = [{ name: 'Hub', repo_id: 'r1' }];
  const workspaceGraph = {
    nodes: [
      { id: 'r1-h', name: 'Hub',    repo_id: 'r1' },
      { id: 'r2-x', name: 'HubExt', repo_id: 'r2' },
    ],
    edges: [],
  };
  const out = computeAnchorVisibleSet(anchors, workspaceGraph);
  const ids = new Set(out.nodes.map(n => n.id));
  assert.ok(ids.has('r1-h'));
  assert.ok(!ids.has('r2-x'), 'r2 node must NOT bleed into a r1-scoped anchor view');
});
```

- [ ] **Step 4: Run the unit tests**

```bash
node --test tests/js/graph_tab.test.js
```

Expected: ≥ 35 pass (32 prior + 3 new). All 3 new tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/server/mcp/command_center/app.js tests/js/graph_tab.test.js
git commit -m "feat(spa): applyFocalGraph helper + tests (Items 13/14 data)"
```

---

## Task 2: SPA integration — `renderGraphTab` branches + click handler + search wiring

**Files:**
- Modify: `src/server/mcp/command_center/app.js` (anchor branch + focal branch + click handler + search-input handler + `graphState` field).
- Modify: `docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}` — re-recorded later (this task doesn't touch them; Task 3 does).

**Interfaces:**
- Consumes: `applyFocalGraph(payload, focalSymbol)` (Task 1); `computeAnchorVisibleSet` (existing); `workspaceGraph` (existing); `graphState.focalSymbol` (existing); `graphState.focalRepoId` (Task 2 introduces).
- Produces: `renderGraphTab` calls the right MCP tool with the right arguments and parses the right shape.

- [ ] **Step 1: Locate the existing `graphState` declaration**

Find the file-scope `const graphState = { ... };` block (added by the v2 plan). It's somewhere in the graph section, just above `renderGraphTab`. Add `focalRepoId: null` to it:

```js
const graphState = {
  mode: 'anchor',          // 'anchor' | 'focal'
  focalSymbol: null,
  focalRepoId: null,       // <-- new: set by the click/search handlers, read by the focal branch
  depth: 1,
  searchQuery: '',
  workspaceGraph: null,
};
```

- [ ] **Step 2: Update the anchor branch in `renderGraphTab`**

Find the `mcpCall('find_anchors', { limit: 30 });` line (added by Task 4 round 2; should be in the anchor branch). Replace with:

```js
// Item 14: find_anchors requires repo_id (the federation's scope guard
// rejects unscoped calls). Pick the first distinct repo_id from the
// already-fetched workspace graph.
const anchorRepoId = (() => {
  for (const n of workspaceGraph.nodes || []) {
    if (n.repo_id) return n.repo_id;
  }
  return null;
})();
const anchorsResult = await mcpCall('find_anchors', {
  repo_id: anchorRepoId,
  limit: 30,
});
```

The text-format parser (`/^\s*\d+\.\s+.../gm`) immediately below stays unchanged — it works on `find_anchors`'s numbered-text payload.

If `anchorRepoId` is null, the existing `anc.length === 0` empty-state hint fires (existing fallback at `app.js:1642-1645`). No change needed for the null case.

- [ ] **Step 3: Update the focal branch in `renderGraphTab`**

Find the focal branch (added by v2 Task 4; it calls `mcpCall('get_blast_radius', { symbol: focalSymbol, depth: String(focalDepth) })`). Replace the entire focal block (from the `mcpCall` through the `drawGraphSvg` call) with:

```js
if (mode === 'focal') {
  const focalSymbol = graphState.focalSymbol;
  const focalDepth  = graphState.depth || 1;
  // Item 13: get_blast_radius returns text and ignores depth; switch to
  // get_cross_repo_blast_radius_for_repo (JSON, accepts depth range).
  let focalRepoId = graphState.focalRepoId;
  if (!focalRepoId) {
    // Fallback for the search-input path where the typed name's repo
    // wasn't captured: take the first distinct repo_id from the cached
    // workspace graph.
    const wg = graphState.workspaceGraph;
    if (wg && wg.nodes) {
      for (const n of wg.nodes) {
        if (n.repo_id) { focalRepoId = n.repo_id; break; }
      }
    }
  }
  if (!focalSymbol) {
    return renderGraphTab();
  }
  empty.textContent = `Loading ${focalSymbol}'s ${focalDepth}-hop neighbourhood…`;
  svg.innerHTML = '';
  let result;
  try {
    result = await mcpCall('get_cross_repo_blast_radius_for_repo', {
      repo_id: focalRepoId,
      symbol: focalSymbol,
      depth: `${focalDepth}..${focalDepth}`,   // "1..1", "2..2", "3..3"
    });
  } catch (e) {
    renderGraphTabEmpty({ mode: 'error', message: `get_cross_repo_blast_radius_for_repo failed: ${e.message}` }, list);
    return;
  }
  if (result && result.isError) {
    const msg = unwrapText(result) || 'error';
    renderGraphTabEmpty({ mode: 'error', message: msg }, list);
    return;
  }
  const focalJson = parseJson(result);
  if (!focalJson || !focalJson.by_repo) {
    renderGraphTabEmpty({ mode: 'error', message: `focal payload unparseable for ${focalSymbol}` }, list);
    return;
  }
  const visible = applyFocalGraph(focalJson, focalSymbol);
  // Wrap into the shape normalizeGraphPayload + drawGraphSvg expect: a
  // graph with nodes, edges, truncated. applyFocalGraph already does
  // that; pass through.
  const graph = {
    nodes: visible.nodes,
    edges: visible.edges,
    truncated: visible.truncated,
  };
  if (meta) {
    const cross = graph.edges.filter(e => e.cross_repo).length;
    meta.textContent = `focal: ${focalSymbol} · ${graph.nodes.length} nodes · ${graph.edges.length} edges · ${cross} cross-repo${graph.truncated ? ' · truncated' : ''}`;
  }
  empty.textContent = '';
  drawGraphSvg(svg, graph);
  return;
}
```

This replaces the prior focal branch (Item 13's fix in concrete form). The key switch is `get_blast_radius` (text, no depth) → `get_cross_repo_blast_radius_for_repo` (JSON, accepts `depth: "1..1"` etc., cross-repo by design).

- [ ] **Step 4: Update the click handler in `drawGraphSvg`**

Find the click handler (added by v2 Task 4 round 1):

```js
.on('click', (event, d) => {
  graphState.focalSymbol = d.name;
  graphState.mode = 'focal';
  renderGraphTab();
});
```

Replace with:

```js
.on('click', (event, d) => {
  graphState.focalSymbol = d.name;
  graphState.focalRepoId  = d.repo_id;             // captured for focal branch's repo_id arg
  graphState.mode        = 'focal';
  renderGraphTab();
});
```

- [ ] **Step 5: Update the search-input handler in `wireGraphControls`**

Find the search handler (added by v2 Task 4 round 1):

```js
debounce = setTimeout(() => {
  const q = (search.value || '').trim();
  if (!q) return;
  state.focalSymbol = q;
  state.mode = 'focal';
  onChange({ restoreFromSearch: q });
}, 300);
```

Replace with:

```js
debounce = setTimeout(() => {
  const q = (search.value || '').trim();
  if (!q) return;
  // Match typed name against the cached workspace graph to set
  // focalRepoId; fall back to the first distinct repo_id if no match
  // (or if the graph is uncached — focalRepoId is null, branch's fallback
  // takes over).
  let typedRepoId = null;
  const wg = state.workspaceGraph;
  if (wg && wg.nodes) {
    for (const n of wg.nodes) {
      if (n && n.name === q) { typedRepoId = n.repo_id || null; break; }
    }
    if (!typedRepoId) {
      for (const n of wg.nodes) {
        if (n && n.repo_id) { typedRepoId = n.repo_id; break; }
      }
    }
  }
  state.focalSymbol = q;
  state.focalRepoId  = typedRepoId;
  state.mode        = 'focal';
  onChange({ restoreFromSearch: q });
}, 300);
```

The argument name `state` here is from the prior plan's `wireGraphControls` signature (`function wireGraphControls(state, onChange)`); if the closure-local variable for graphState is named differently in the current code, the field assignments (`state.focalSymbol`, `state.focalRepoId`) should read as `state.focalSymbol` / `state.focalRepoId` regardless of which reference identifies the closure-local. Verify against the current code with `grep -n 'wireGraphControls'` first.

- [ ] **Step 6: Rebuild the Rust binary**

`app.js` is `include_bytes!`'d. Without a rebuild, the recorder / e2e see stale code.

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo build --release --quiet
```

- [ ] **Step 7: Run the unit tests + a smoke federation probe**

```bash
node --test tests/js/graph_tab.test.js
bash scripts/smoke_federation_fixture.sh
```

Expected: graph_tab tests pass; smoke test OK with the latest spec (network permitting).

- [ ] **Step 8: Commit**

```bash
git add src/server/mcp/command_center/app.js
git commit -m "fix(spa): Item 13/14 — pass repo_id to find_anchors; switch focal to get_cross_repo_blast_radius_for_repo"
```

---

## Task 3: SPA e2e assertion + recorder re-record + final verify + push

**Files:**
- Modify: `tests/js/spa_e2e.test.js` (1 new assertion).
- Modify: `docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}` (re-recorded, NOT yet committed — that's Step 6).

- [ ] **Step 1: Append 1 new Graph-tab assertion in `tests/js/spa_e2e.test.js`**

Find the existing Graph-tab block (added by the v2 plan's Task 5). After the existing v2 assertions (around line 487 in the current file), append:

```js
// Item 13/14 follow-on: focal mode renders nodes for the queried symbol
// (after Item 13's tool swap to get_cross_repo_blast_radius_for_repo).
const v2FocalRendered = await page.evaluate(() => {
  const nodes = document.querySelectorAll('.graph-node');
  const meta = document.getElementById('graph-meta');
  return {
    nodeCount: nodes.length,
    metaText: meta ? meta.textContent : '',
  };
});
assert.ok(
  v2FocalRendered.nodeCount > 0,
  'focal mode rendered nodes for the queried symbol (got ' + v2FocalRendered.nodeCount + ')',
);
```

This lives near the existing v2 assertions; verify the actual line range with `grep -n 'v2ClickResetsCount\|focal mode' tests/js/spa_e2e.test.js` and insert at the right spot.

- [ ] **Step 2: Run the e2e suite**

```bash
node tests/js/spa_e2e.test.js 2>&1 | tail -20
```

Expected: 49/49 pass (48 prior + 1 new).

- [ ] **Step 3: Re-record the hero**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
export LAIN_REINDEX_TIMEOUT=600
./scripts/record-spa-demo.sh --json /tmp/lain-record-summary.json
cat /tmp/lain-record-summary.json
ls -la docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}
```

Allow up to 10 minutes (recorder cap is 600 s per the prior fix wave).

- [ ] **Step 4: Extract a frame at t≈35 s and save to the SDD workspace**

The brief said t≈25 s when focal activated immediately after the Graph click. With the v2 floor and Task 7's 6 s + 3 s wait, the focal view settles around t≈25-30 s of the Graph tab. In recording time (which includes terminal boot + federation probe), t≈30-35 s of the GIF is the focal-settled frame. Use t≈35 s as a safe capture point.

```bash
TMP="$(mktemp -d)"
ffmpeg -y -hide_banner -loglevel error -ss 35 -i docs/screenshots/spa-demo.gif -frames:v 1 "$TMP/graph-frame.png"
cp "$TMP/graph-frame.png" .superpowers/sdd/2026-09-01-spa-graph-mcp-text-fixes/task-3-graph-frame.png
ls -la "$TMP"
rm -rf "$TMP"
```

(Wrong SDD dir; pick the new plan's dir. The existing v2 SDD dir already has its own; the new plan's dir will be `.superpowers/sdd/2026-09-01-spa-graph-mcp-text-fixes/` once it's set up — Task 3's frame extraction may need the SDD workspace created in advance. If the dir doesn't exist yet, fall back to saving to `/tmp/lain-graph-frame.png` and surfacing the location in the report.)

- [ ] **Step 5: Commit the e2e assertion + the four hero artifacts**

```bash
git add tests/js/spa_e2e.test.js \
        docs/screenshots/spa-demo.webm \
        docs/screenshots/spa-demo.mp4 \
        docs/screenshots/spa-demo.gif \
        docs/screenshots/spa-demo-poster.png
git commit -m "fix(recording): Item 13/14 — focal hero now shows actual blast-radius nodes+edges"
```

- [ ] **Step 6: Final verification + push**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --test cli_surface 2>&1 | tail -10
node --test tests/js/graph_tab.test.js 2>&1 | tail -10
node tests/js/spa_e2e.test.js 2>&1 | tail -10
git push -u origin main
git log --oneline origin/main -10
ls -la docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}
```

Expected: cli_surface 3/3, graph_tab 35/35 (32 + 3 new), spa_e2e 49/49 (48 + 1 new). Push lands the 3 new commits on top of the prior fix-wave branches.

---

## Self-Review (controller-only)

**Spec coverage:**

- ✅ Item 14: anchor branch passes `repo_id` — Task 2 Step 2.
- ✅ Item 13: focal branch switches to `get_cross_repo_blast_radius_for_repo` — Task 2 Step 3.
- ✅ `applyFocalGraph` pure helper + 3 tests — Task 1 Steps 1, 3.
- ✅ `graphState.focalRepoId` field + click handler + search handler — Task 2 Steps 1, 4, 5.
- ✅ Cargo rebuild between JS edit and test — Task 2 Step 6.
- ✅ 1 new SPA e2e assertion — Task 3 Step 1.
- ✅ Hero re-record + frame extraction — Task 3 Steps 3-4.
- ✅ Final verification + push — Task 3 Step 6.

**Placeholder scan:** zero TBD / TODO / FIXME.

**Type/symbol consistency:**

- `applyFocalGraph(payload, focalSymbol)` — defined Task 1 Step 1, used Task 2 Step 3.
- `graphState.focalRepoId` — declared Task 2 Step 1, written Task 2 Steps 4 + 5, read Task 2 Step 3.
- `anchorRepoId` (local const in anchor branch) — declared Task 2 Step 2, used in the `mcpCall` immediately below.
- `focalRepoId` (local const in focal branch) — declared Task 2 Step 3, used in `mcpCall` immediately below. **Note:** this shadows the click-handler-set `graphState.focalRepoId` field name; the local const uses `let focalRepoId = ...` with a fallback to `workspaceGraph.nodes[0].repo_id`, distinct from the closure-field name. To make this unambiguous, the focal branch's local const could be renamed `focalRepoIdResolved` — but a let-variable shadowing the closure field name is syntactically fine. Task implementer may rename for clarity.
- Tool names: `find_anchors` (anchor), `get_cross_repo_blast_radius_for_repo` (focal). Uniform across Tasks 2 and 3.
- `repo_id` value: the first distinct non-null `repo_id` from `workspaceGraph.nodes`. Uniform across the workspace.

No internal contradictions found.
