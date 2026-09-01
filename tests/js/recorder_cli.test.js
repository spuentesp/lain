// Regression guard for the recorder CLI's argument parser. The
// wait-for-ready cap is the longest single timeout in the recorder
// pipeline and is the knob cold-cache operators reach for first
// when federation reindex blows past the default. Pinning the
// parser shape here means a future refactor of record_spa_demo.js
// can't silently drop the flag.
//
// Run: node --test tests/js/recorder_cli.test.js

const test = require('node:test');
const assert = require('node:assert');

// We re-parse the file as text rather than requiring it: the
// recorder's main() does heavy work (spawns lain, launches
// Playwright); we just want the arg-parsing branch. The simplest
// path is to read the source and assert on the structural
// invariants we care about.
const fs = require('node:fs');
const path = require('node:path');
const SRC = fs.readFileSync(
  path.resolve(__dirname, 'record_spa_demo.js'),
  'utf8',
);

test('recorder parseArgs defaults include ready_timeout_ms = 600_000', () => {
  // The default object literal must include `ready_timeout_ms: 600_000`.
  assert.match(SRC, /ready_timeout_ms:\s*600_000/,
    'parseArgs defaults should expose ready_timeout_ms: 600_000');
});

test('recorder parseArgs switch recognises --ready-timeout-ms', () => {
  // The parser loop must accept the long-form flag.
  assert.match(SRC, /--ready-timeout-ms/,
    'parseArgs should accept --ready-timeout-ms');
});

test('recorder passes ready_timeout_ms into waitForReady (no hard-coded 600_000)', () => {
  // The hard-coded literal 600_000 was the original pain point.
  // After this task, the call site should pass args.ready_timeout_ms,
  // NOT a literal. We assert that no `waitForReady(...600_000)` call
  // remains in the file.
  assert.doesNotMatch(SRC, /waitForReady\([^)]*600_000/,
    'waitForReady call should use args.ready_timeout_ms, not a literal 600_000');
});
