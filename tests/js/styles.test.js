// Regression guard for duplicate CSS selector declarations. The Command
// Center SPA is served via `include_bytes!`, so an accidental paste of
// an existing rule (the most common drift) produces a silently
// override-the-first rule rather than a build error.
//
// Run: node --test tests/js/

const test = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const path = require('node:path');

const CSS_PATH = path.resolve(
  __dirname, '../../src/server/mcp/command_center/styles.css',
);

function countSelectorOccurrences(cssText, selectorRegex) {
  // Match only top-of-line declarations so we don't count nested
  // `.graph-link.foo { ... }` etc. — those are different rules.
  const re = new RegExp('^' + selectorRegex.source, 'gm');
  return (cssText.match(re) || []).length;
}

test('styles.css: .graph-link is declared exactly once', () => {
  const css = fs.readFileSync(CSS_PATH, 'utf8');
  const n = countSelectorOccurrences(css, /\.graph-link\s*\{/);
  assert.strictEqual(n, 1,
    `.graph-link { should appear once but appears ${n} times — ` +
    `the second declaration silently shadows the first`);
});

test('styles.css: .graph-link.cross-repo is declared exactly once', () => {
  const css = fs.readFileSync(CSS_PATH, 'utf8');
  const n = countSelectorOccurrences(css, /\.graph-link\.cross-repo\s*\{/);
  assert.strictEqual(n, 1,
    `.graph-link.cross-repo { should appear once but appears ${n} times`);
});
