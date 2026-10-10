const test = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { parseCast, selectKeyFrames } = require('./cast-to-png.js');

// Grid rows are Maps keyed by column; flatten to plain terminal text so the
// assertions read as screen content.
function gridText(grid) {
  return grid
    .map(row => [...row.keys()].sort((a, b) => a - b).map(c => row.get(c).char).join(''))
    .join('\n')
    .replace(/\u00a0/g, ' ');
}

function writeCast(events) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cast-to-png-'));
  const castPath = path.join(dir, 'session.cast');
  const header = JSON.stringify({ version: 2, width: 80, height: 24 });
  const body = events.map(e => JSON.stringify(e)).join('\n');
  fs.writeFileSync(castPath, `${header}\n${body}\n`);
  return castPath;
}

test('selectKeyFrames: each sample shows only the output elapsed by its time', () => {
  const cast = parseCast(writeCast([
    [0.5, 'o', 'alpha\r\n'],
    [1.5, 'o', 'beta\r\n'],
    [2.5, 'o', 'gamma\r\n'],
  ]));
  const texts = selectKeyFrames(cast.frames, 1).map(k => gridText(k.grid));

  assert.equal(texts[0], '');
  assert.equal(texts[1], 'alpha');
  assert.equal(texts[2], 'alpha\nbeta');
  assert.equal(texts[3], 'alpha\nbeta\ngamma');
  assert.notEqual(texts[1], texts[texts.length - 1],
    'an early sample must differ from a late one');
});

test('selectKeyFrames: trailing silence holds the final state', () => {
  const cast = parseCast(writeCast([[0.5, 'o', 'only\r\n']]));
  const texts = selectKeyFrames(cast.frames, 1).map(k => gridText(k.grid));

  assert.ok(texts.length >= 3);
  assert.equal(texts[texts.length - 1], 'only');
  assert.equal(texts[texts.length - 2], texts[texts.length - 1]);
});

test('parseCast: reads the header size and output events only', () => {
  const cast = parseCast(writeCast([
    [0.1, 'o', 'hello'],
    [0.2, 'i', 'ignored'],
  ]));
  assert.equal(cast.width, 80);
  assert.equal(cast.height, 24);
  assert.equal(cast.frames.length, 1);
  assert.equal(cast.frames[0].data, 'hello');
});
