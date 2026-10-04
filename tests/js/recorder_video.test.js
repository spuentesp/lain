const test = require('node:test');
const assert = require('node:assert');
const app = require('./record_spa_demo_video.js');

test('federationIsReady: rejects a missing or malformed federation', () => {
  assert.equal(app.federationIsReady(null, 2), false);
  assert.equal(app.federationIsReady({}, 2), false);
  assert.equal(app.federationIsReady({ federation: {} }, 2), false);
  assert.equal(app.federationIsReady({ federation: { repos: 'x' } }, 2), false);
});

test('federationIsReady: requires every repo to be ready', () => {
  const all = { federation: { repos: [{ id: 'a', health: 'ready' }, { id: 'b', health: 'ready' }] } };
  const mixed = { federation: { repos: [{ id: 'a', health: 'ready' }, { id: 'b', health: 'indexing' }] } };
  assert.equal(app.federationIsReady(all, 2), true);
  assert.equal(app.federationIsReady(mixed, 2), false);
});

test('federationIsReady: respects minRepos', () => {
  // A leftover server serving 1 repo must not satisfy a 2-repo fixture,
  // and a 3-repo fixture must not be satisfied by 2.
  const two = { federation: { repos: [{ id: 'a', health: 'ok' }, { id: 'b', health: 'ok' }] } };
  assert.equal(app.federationIsReady(two, 1), true);
  assert.equal(app.federationIsReady(two, 3), false);
  assert.equal(app.federationIsReady(two, 2), true);
});

test('federationIsReady: treats health "ok" as ready', () => {
  const body = { federation: { repos: [{ id: 'a', health: 'ok' }] } };
  assert.equal(app.federationIsReady(body, 1), true);
});

test('federationReadyWithin: resolves true once the federation turns ready', async () => {
  const origFetch = globalThis.fetch;
  let calls = 0;
  globalThis.fetch = async () => {
    calls++;
    const body = calls < 2
      ? { federation: { repos: [{ id: 'a', health: 'indexing' }, { id: 'b', health: 'ready' }] } }
      : { federation: { repos: [{ id: 'a', health: 'ready' }, { id: 'b', health: 'ready' }] } };
    return { status: 200, json: async () => body };
  };
  try {
    assert.equal(await app.federationReadyWithin('http://127.0.0.1:1', 2, 5000), true);
    assert.ok(calls >= 2, 'must poll again after a not-ready answer');
  } finally {
    globalThis.fetch = origFetch;
  }
});

test('federationReadyWithin: gives up at the deadline', async () => {
  const origFetch = globalThis.fetch;
  globalThis.fetch = async () => ({
    status: 200,
    json: async () => ({ federation: { repos: [{ id: 'a', health: 'ready' }] } }),
  });
  try {
    assert.equal(await app.federationReadyWithin('http://127.0.0.1:1', 2, 350), false);
  } finally {
    globalThis.fetch = origFetch;
  }
});
