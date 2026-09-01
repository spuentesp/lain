// Unit tests for the pure helpers in the Command Center SPA.
//
// Run: node --test tests/js/
//
// app.js is a browser script, not a module. It is loadable here because its
// bottom guard skips init() when there is no `document`, and exports its pure
// helpers under CommonJS when `module` exists. No DOM, no jsdom, no npm deps.

const test = require('node:test');
const assert = require('node:assert');

const app = require('../../src/server/mcp/command_center/app.js');

test('app.js loads under Node without a DOM', () => {
  assert.ok(app, 'app.js exported nothing');
  assert.strictEqual(typeof app.collapseBursts, 'function');
});

test('collapseBursts groups a same-path burst inside the window', () => {
  const base = 1000000;
  const cards = app.collapseBursts([
    { ts: base, path: '/x.rs' },
    { ts: base + 1000, path: '/x.rs' },
    { ts: base + 2000, path: '/x.rs' },
  ], { window_ms: 5000 });
  assert.strictEqual(cards.length, 1);
  assert.strictEqual(cards[0].count, 3);
});

test('collapseBursts keeps events outside the window separate', () => {
  const base = 1000000;
  const cards = app.collapseBursts([
    { ts: base, path: '/x.rs' },
    { ts: base + 6000, path: '/x.rs' },
  ], { window_ms: 5000 });
  assert.strictEqual(cards.length, 2);
});

// ── pickWorkspaceForGraph (D-M8) ────────────────────────────────────────────

test('pickWorkspaceForGraph: no workspaces -> none', () => {
  assert.deepStrictEqual(app.pickWorkspaceForGraph([]), {
    mode: 'none', workspace: null,
  });
});

test('pickWorkspaceForGraph: non-array input -> none', () => {
  assert.deepStrictEqual(app.pickWorkspaceForGraph(null), {
    mode: 'none', workspace: null,
  });
  assert.deepStrictEqual(app.pickWorkspaceForGraph(undefined), {
    mode: 'none', workspace: null,
  });
});

test('pickWorkspaceForGraph: exactly one workspace auto-selects it', () => {
  assert.deepStrictEqual(
    app.pickWorkspaceForGraph([{ name: 'solo', member_count: 1, is_active: false }]),
    { mode: 'auto', workspace: 'solo' },
  );
});

test('pickWorkspaceForGraph: the active workspace wins over count', () => {
  assert.deepStrictEqual(
    app.pickWorkspaceForGraph([
      { name: 'a', is_active: false },
      { name: 'b', is_active: true },
      { name: 'c', is_active: false },
    ]),
    { mode: 'auto', workspace: 'b' },
  );
});

test('pickWorkspaceForGraph: many workspaces, none active -> picker', () => {
  assert.deepStrictEqual(
    app.pickWorkspaceForGraph([
      { name: 'a', is_active: false },
      { name: 'b', is_active: false },
    ]),
    { mode: 'picker', workspace: null },
  );
});

test('pickWorkspaceForGraph: entries without a name are ignored', () => {
  assert.deepStrictEqual(
    app.pickWorkspaceForGraph([{ member_count: 3 }, { name: 'real' }]),
    { mode: 'auto', workspace: 'real' },
  );
});

// ── classifyWorkspacesResult (D-M8) ─────────────────────────────────────────

test('classifyWorkspacesResult: a healthy list passes through', () => {
  const out = app.classifyWorkspacesResult(
    { content: [{ type: 'text', text: '[]' }] },
    [{ name: 'solo' }],
  );
  assert.strictEqual(out.ok, true);
  assert.strictEqual(out.configless, false);
  assert.deepStrictEqual(out.list, [{ name: 'solo' }]);
});

test('classifyWorkspacesResult: "Unknown tool" is a config state, not an error', () => {
  const out = app.classifyWorkspacesResult(
    { isError: true, content: [{ type: 'text', text: 'Unknown tool: list_workspaces' }] },
    null,
  );
  assert.strictEqual(out.ok, true, 'a configless federation is not a failure');
  assert.strictEqual(out.configless, true);
  assert.deepStrictEqual(out.list, []);
});

test('classifyWorkspacesResult: "no workspaces file" is also a config state', () => {
  const out = app.classifyWorkspacesResult(
    { isError: true, content: [{ type: 'text', text: 'no workspaces file loaded' }] },
    null,
  );
  assert.strictEqual(out.ok, true);
  assert.strictEqual(out.configless, true);
});

test('classifyWorkspacesResult: a real tool error is surfaced', () => {
  const out = app.classifyWorkspacesResult(
    { isError: true, content: [{ type: 'text', text: 'index poisoned' }] },
    null,
  );
  assert.strictEqual(out.ok, false);
  assert.strictEqual(out.configless, false);
  assert.match(out.message, /index poisoned/);
});

test('classifyWorkspacesResult: an unparseable body yields an empty list', () => {
  const out = app.classifyWorkspacesResult(
    { content: [{ type: 'text', text: 'not json' }] },
    null,
  );
  assert.strictEqual(out.ok, true);
  assert.deepStrictEqual(out.list, []);
});

// ── normalizeGraphPayload (D-M8) ────────────────────────────────────────────

test('normalizeGraphPayload: a well-formed payload survives intact', () => {
  const out = app.normalizeGraphPayload({
    nodes: [
      { id: 'r::a', name: 'a', path: 'src/a.rs', repo_id: 'r', kind: 'Function' },
      { id: 'r::b', name: 'b', path: 'src/b.rs', repo_id: 'r', kind: 'Function' },
    ],
    edges: [{ source: 'r::a', target: 'r::b', edge_type: 'Calls', cross_repo: false }],
    truncated: false,
  });
  assert.strictEqual(out.nodes.length, 2);
  assert.strictEqual(out.edges.length, 1);
  assert.strictEqual(out.truncated, false);
});

test('normalizeGraphPayload: drops edges with a dangling endpoint', () => {
  const out = app.normalizeGraphPayload({
    nodes: [{ id: 'r::a', name: 'a', path: '', repo_id: 'r', kind: 'Function' }],
    edges: [
      { source: 'r::a', target: 'r::gone', edge_type: 'Calls' },
      { source: 'r::missing', target: 'r::a', edge_type: 'Calls' },
    ],
  });
  assert.strictEqual(out.edges.length, 0, 'd3.forceLink throws on unknown node ids');
});

test('normalizeGraphPayload: drops nodes without an id', () => {
  const out = app.normalizeGraphPayload({
    nodes: [{ name: 'nameless' }, { id: 'r::a', name: 'a' }],
    edges: [],
  });
  assert.strictEqual(out.nodes.length, 1);
  assert.strictEqual(out.nodes[0].id, 'r::a');
});

test('normalizeGraphPayload: null / garbage yields an empty graph', () => {
  assert.deepStrictEqual(app.normalizeGraphPayload(null),
    { nodes: [], edges: [], truncated: false });
  assert.deepStrictEqual(app.normalizeGraphPayload({ nodes: 'x', edges: 7 }),
    { nodes: [], edges: [], truncated: false });
});

test('normalizeGraphPayload: preserves the truncation flag', () => {
  const out = app.normalizeGraphPayload({ nodes: [], edges: [], truncated: true });
  assert.strictEqual(out.truncated, true);
});

// ── SPA graph upgrade (2026-08-31): pure helpers for filters / palette / shape ──

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
  const palette = app.computeRepoPalette(graph);
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
  assert.equal(app.repoColour('bytes', palette),   'graph-repo-a');
  assert.equal(app.repoColour('unknown', palette), 'graph-repo-fallback');
  assert.equal(app.repoColour('', palette),        'graph-repo-fallback');
  assert.equal(app.repoColour(null, palette),      'graph-repo-fallback');
});

test('nodeShape: round-trips Function/Method/Class + unknown fallback', () => {
  // Helper returns the d3-symbol namespace key, so callers can do
  // `d3[nodeShape(kind)]` directly. The shape-by-kind mapping is
  // documented here as the helper's contract.
  assert.equal(app.nodeShape('Function'), 'symbolCircle');
  assert.equal(app.nodeShape('Method'),   'symbolDiamond');
  assert.equal(app.nodeShape('Class'),    'symbolSquare');
  assert.equal(app.nodeShape(''),         'symbolCircle');
  assert.equal(app.nodeShape('Trait'),    'symbolCircle');  // defensive
  assert.equal(app.nodeShape(undefined),  'symbolCircle');
});

test('nodeRadius: 5/6/7 by role', () => {
  assert.equal(app.nodeRadius('default'),   5);
  assert.equal(app.nodeRadius('neighbour'), 6);
  assert.equal(app.nodeRadius('focus'),     7);
  assert.equal(app.nodeRadius(undefined),   5);
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
  const out = app.applyFilters(graph, state);
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
  const out = app.applyFilters(graph, state);
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
  const out = app.applyFilters(graph, state);
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
  const out = app.applyFilters(graph, state);
  assert.equal(out.visibleEdges.length, 0);
  assert.equal(out.visibleNodes.length, 1);
});

// ── computeAnchorVisibleSet / applyDepth (v2, 2026-08-31) ──────────────────

test('computeAnchorVisibleSet: anchor with no incident edges is itself visible', () => {
  const anchors = [{ name: 'orphan', repo_id: 'r1' }];
  const workspaceGraph = {
    nodes: [{ id: 'a', name: 'orphan', repo_id: 'r1', kind: 'Function' },
            { id: 'b', name: 'others', repo_id: 'r1', kind: 'Function' }],
    edges: [{ source: 'b', target: 'b2', cross_repo: false }],
  };
  const out = app.computeAnchorVisibleSet(anchors, workspaceGraph);
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
  const out = app.computeAnchorVisibleSet(anchors, workspaceGraph);
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
  const out = app.computeAnchorVisibleSet(anchors, { nodes, edges }, { maxNeighboursPerAnchor: 5 });
  // 1 anchor + capped 5 neighbours = 6 visible.
  assert.equal(out.nodes.length, 6);
  const visibleIds = new Set(out.nodes.map(n => n.id));
  assert.ok(visibleIds.has('hub'));
  const neighbours = out.nodes.filter(n => n.id !== 'hub');
  assert.equal(neighbours.length, 5);
});

test('computeAnchorVisibleSet: anchors with repo_id=null match by name only across repos', () => {
  // Anchor has no repo_id; should match nodes named "Bytes" in any repo.
  const anchors = [{ name: 'Bytes', repo_id: null }];
  const workspaceGraph = {
    nodes: [
      { id: 'a', name: 'Bytes', repo_id: 'r1' },
      { id: 'b', name: 'Bytes', repo_id: 'r2' },
      { id: 'c', name: 'Other', repo_id: 'r1' },
    ],
    edges: [],
  };
  const out = app.computeAnchorVisibleSet(anchors, workspaceGraph);
  const ids = new Set(out.nodes.map(n => n.id));
  assert.ok(ids.has('a'));
  assert.ok(ids.has('b'));
  assert.ok(!ids.has('c'));
});

test('computeAnchorVisibleSet: anchor with explicit repo_id matches only that repo', () => {
  const anchors = [{ name: 'Bytes', repo_id: 'r1' }];
  const workspaceGraph = {
    nodes: [
      { id: 'a', name: 'Bytes', repo_id: 'r1' },
      { id: 'b', name: 'Bytes', repo_id: 'r2' },
    ],
    edges: [],
  };
  const out = app.computeAnchorVisibleSet(anchors, workspaceGraph);
  const ids = new Set(out.nodes.map(n => n.id));
  assert.ok(ids.has('a'));
  assert.ok(!ids.has('b'));
});
