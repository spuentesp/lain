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
