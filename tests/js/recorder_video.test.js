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
