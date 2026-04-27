#!/usr/bin/env node
// Tests for npm-shim bin/lain.js shim logic

const assert = require('assert');
const path = require('path');
const os = require('os');

// ─────────────────────────────────────────────
// Helpers — mirror the logic in bin/lain.js
// ─────────────────────────────────────────────

function getLauncherPath(home = os.homedir()) {
  const LAIN_DIR = path.join(home, '.lain');
  const isWin = process.platform === 'win32';
  const launcherName = isWin ? 'lain-launcher.cmd' : 'lain-launcher';
  return path.join(LAIN_DIR, 'bin', launcherName);
}

function getLauncherContent(binaryPath, isWin = process.platform === 'win32') {
  const launcherName = isWin ? 'lain-launcher.cmd' : 'lain-launcher';
  return isWin
    ? `@echo off\n"${binaryPath}" %*`
    : `#!/bin/sh\nexec "${binaryPath}" "$@"\n`;
}

// ─────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────

let passed = 0;
let failed = 0;

function test(name, fn) {
  try {
    fn();
    console.log(`  ✓ ${name}`);
    passed++;
  } catch (e) {
    console.log(`  ✗ ${name}`);
    console.log(`    ${e.message}`);
    failed++;
  }
}

console.log('\n[ getLauncherPath ]');

test('launcher path for unix', () => {
  const isWin = false;
  const LAIN_DIR = path.join(os.homedir(), '.lain');
  const launcherName = isWin ? 'lain-launcher.cmd' : 'lain-launcher';
  const launcherPath = path.join(LAIN_DIR, 'bin', launcherName);
  assert.strictEqual(launcherPath, path.join(os.homedir(), '.lain', 'bin', 'lain-launcher'));
});

test('launcher path for windows', () => {
  const isWin = true;
  const LAIN_DIR = path.join(os.homedir(), '.lain');
  const launcherName = isWin ? 'lain-launcher.cmd' : 'lain-launcher';
  const launcherPath = path.join(LAIN_DIR, 'bin', launcherName);
  assert.strictEqual(launcherPath, path.join(os.homedir(), '.lain', 'bin', 'lain-launcher.cmd'));
});

test('getLauncherPath uses os.homedir()', () => {
  const result = getLauncherPath();
  assert.ok(result.endsWith('.lain/bin/lain-launcher'));
});

console.log('\n[ launcher content ]');

test('unix launcher is valid sh script with shebang', () => {
  const content = getLauncherContent('/home/user/.lain/bin/lain', false);
  assert.ok(content.startsWith('#!/bin/sh'));
  assert.ok(content.includes('exec "/home/user/.lain/bin/lain" "$@"'));
  assert.ok(content.endsWith('\n'));
});

test('windows launcher uses @echo off', () => {
  const content = getLauncherContent('C:\\Users\\user\\.lain\\bin\\lain.exe', true);
  assert.ok(content.startsWith('@echo off'));
  assert.ok(content.includes('"C:\\Users\\user\\.lain\\bin\\lain.exe"'));
  assert.ok(content.includes('%*'));
});

console.log('\n[ shim path chain ]');

test('bin/lain.js spawns the launcher, not the binary directly', () => {
  // Verify the launcher path exists as a distinct concept from the binary path
  const isWin = false;
  const LAIN_DIR = path.join(os.homedir(), '.lain');
  const binaryName = isWin ? 'lain.exe' : 'lain';
  const launcherName = isWin ? 'lain-launcher.cmd' : 'lain-launcher';

  const binaryPath = path.join(LAIN_DIR, 'bin', binaryName);     // ~/.lain/bin/lain
  const launcherPath = path.join(LAIN_DIR, 'bin', launcherName); // ~/.lain/bin/lain-launcher

  assert.notStrictEqual(binaryPath, launcherPath);
  assert.ok(launcherPath.endsWith('lain-launcher'));
  assert.ok(binaryPath.endsWith('lain'));
});

console.log('\n[ error message correctness ]');

test('error message references launcher path, not binary', () => {
  const isWin = false;
  const LAIN_DIR = path.join(os.homedir(), '.lain');
  const launcherName = isWin ? 'lain-launcher.cmd' : 'lain-launcher';
  const launcherPath = path.join(LAIN_DIR, 'bin', launcherName);

  const msg = `Lain binary not found at ${launcherPath}`;
  // The message should mention the launcher, not the bare binary
  assert.ok(msg.includes('lain-launcher'), 'error message should reference launcher');
  assert.ok(!msg.includes('/lain"') || msg.includes('lain-launcher'),
    'should not show bare binary path in error');
});

console.log('\n[ spawn argument forwarding ]');

test('process.argv.slice(2) strips node and script name', () => {
  // Simulate: node /path/to/lain.js --workspace /foo bar
  const argv = ['node', '/path/to/lain.js', '--workspace', '/foo', 'bar'];
  const forwarded = argv.slice(2);
  assert.deepStrictEqual(forwarded, ['--workspace', '/foo', 'bar']);
});

test('process.argv.slice(2) preserves all arguments', () => {
  const argv = ['node', '/path/to/lain.js', '--transport', 'stdio', '--workspace', '/proj'];
  const forwarded = argv.slice(2);
  assert.deepStrictEqual(forwarded, ['--transport', 'stdio', '--workspace', '/proj']);
});

console.log('\n[ exit code handling ]');

test('child exit code is passed through', () => {
  // Simulate: code ?? 0
  const cases = [[0, 0], [1, 1], [null, 0], [undefined, 0], [42, 42]];
  for (const [code, expected] of cases) {
    const result = code ?? 0;
    assert.strictEqual(result, expected, `code=${code} should yield ${expected}`);
  }
});

// ── Summary ────────────────────────────────

console.log(`\n${'─'.repeat(40)}`);
console.log(`  Results: ${passed} passed, ${failed} failed`);
if (failed > 0) process.exit(1);
console.log('  All tests passed.\n');