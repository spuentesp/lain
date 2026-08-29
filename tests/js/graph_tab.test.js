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
