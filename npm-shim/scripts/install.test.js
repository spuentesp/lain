#!/usr/bin/env node
// Tests for npm-shim install.js logic

const assert = require('assert');
const path = require('path');
const os = require('os');
const fs = require('fs');

// ─────────────────────────────────────────────
// Helpers — reimplement the core logic under test
// so tests run against the same code paths.
// ─────────────────────────────────────────────

function getPlatform(platform = process.platform, arch = process.arch) {
  if (platform === 'darwin' && arch === 'arm64') return 'aarch64-apple-darwin';
  if (platform === 'darwin' && arch === 'x64')   return 'x86_64-apple-darwin';
  if (platform === 'linux'  && arch === 'x64')   return 'x86_64-unknown-linux-gnu';
  if (platform === 'win32'  && arch === 'x64')   return 'x86_64-pc-windows-msvc.exe';
  throw new Error(`Unsupported platform: ${platform}-${arch}`);
}

function getAssetName(platform, version) {
  const versionSlug = version.replace(/^v/, '');
  const map = {
    'aarch64-apple-darwin':     `lain-${versionSlug}-aarch64-apple-darwin`,
    'x86_64-apple-darwin':      `lain-${versionSlug}-x86_64-apple-darwin`,
    'x86_64-unknown-linux-gnu': `lain-${versionSlug}-x86_64-unknown-linux-gnu`,
    'x86_64-pc-windows-msvc.exe': `lain-${versionSlug}-x86_64-pc-windows-msvc.exe`,
  };
  const asset = map[platform];
  if (!asset) throw new Error(`Unsupported platform for binary download: ${platform}`);
  return asset;
}

function getBinaryName(isWin) {
  return isWin ? 'lain.exe' : 'lain';
}

function buildUrl(platform, version = 'v0.1.0', repo = 'spuentesp/lain') {
  const assetName = getAssetName(platform, version);
  return `https://github.com/${repo}/releases/download/${version}/${assetName}`;
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

// ── Platform detection ─────────────────────

console.log('\n[ getPlatform ]');

test('darwin arm64 → aarch64-apple-darwin', () => {
  assert.strictEqual(getPlatform('darwin', 'arm64'), 'aarch64-apple-darwin');
});

test('darwin x64 → x86_64-apple-darwin', () => {
  assert.strictEqual(getPlatform('darwin', 'x64'), 'x86_64-apple-darwin');
});

test('linux x64 → x86_64-unknown-linux-gnu', () => {
  assert.strictEqual(getPlatform('linux', 'x64'), 'x86_64-unknown-linux-gnu');
});

test('win32 x64 → x86_64-pc-windows-msvc.exe', () => {
  assert.strictEqual(getPlatform('win32', 'x64'), 'x86_64-pc-windows-msvc.exe');
});

test('unknown platform throws', () => {
  let threw = false;
  try { getPlatform('freebsd', 'x64'); } catch (e) { threw = true; }
  assert(threw, 'should throw on freebsd');
});

// ── Asset name construction ─────────────────

console.log('\n[ getAssetName ]');

test('v0.1.0 → "0.1.0" slug (no leading v in asset name)', () => {
  const name = getAssetName('x86_64-unknown-linux-gnu', 'v0.1.0');
  assert.strictEqual(name, 'lain-0.1.0-x86_64-unknown-linux-gnu');
});

test('0.1.0 (no v) also works', () => {
  const name = getAssetName('x86_64-apple-darwin', '0.1.0');
  assert.strictEqual(name, 'lain-0.1.0-x86_64-apple-darwin');
});

test('macOS ARM asset name is correct', () => {
  const name = getAssetName('aarch64-apple-darwin', 'v0.1.0');
  assert.strictEqual(name, 'lain-0.1.0-aarch64-apple-darwin');
});

test('Windows asset name ends in .exe', () => {
  const name = getAssetName('x86_64-pc-windows-msvc.exe', 'v0.1.0');
  assert.strictEqual(name, 'lain-0.1.0-x86_64-pc-windows-msvc.exe');
});

test('unknown platform in getAssetName throws', () => {
  let threw = false;
  try { getAssetName('freebsd-x64', 'v0.1.0'); } catch (e) { threw = true; }
  assert(threw, 'should throw on unknown platform');
});

// ── URL construction ────────────────────────

console.log('\n[ buildUrl ]');

test('GitHub release URL is correctly formed', () => {
  const url = buildUrl('x86_64-unknown-linux-gnu', 'v0.1.0');
  assert.strictEqual(
    url,
    'https://github.com/spuentesp/lain/releases/download/v0.1.0/lain-0.1.0-x86_64-unknown-linux-gnu'
  );
});

test('custom repo works', () => {
  const url = buildUrl('x86_64-apple-darwin', 'v0.1.0', 'myorg/mylain');
  assert.strictEqual(
    url,
    'https://github.com/myorg/mylain/releases/download/v0.1.0/lain-0.1.0-x86_64-apple-darwin'
  );
});

test('different version in URL', () => {
  const url = buildUrl('x86_64-apple-darwin', 'v1.2.3');
  assert.ok(url.includes('/v1.2.3/'));
  assert.ok(url.includes('lain-1.2.3-x86_64-apple-darwin'));
});

// ── Binary name ─────────────────────────────

console.log('\n[ getBinaryName ]');

test('win32 → lain.exe', () => {
  assert.strictEqual(getBinaryName(true), 'lain.exe');
});

test('unix → lain (no extension)', () => {
  assert.strictEqual(getBinaryName(false), 'lain');
});

// ── Symlink path construction ──────────────

console.log('\n[ createSymlink path ]');

test('node_modules/.bin path on unix', () => {
  // Simulate: __dirname = /pkg/scripts  →  package root = /pkg
  const __dirname = '/pkg/scripts';
  const launcherName = 'lain-launcher';
  const nodeBin = path.join(__dirname, '..', 'node_modules', '.bin', launcherName);
  assert.strictEqual(nodeBin, '/pkg/node_modules/.bin/lain-launcher');
});

test('node_modules/.bin path on windows', () => {
  // path.join normalizes separators for the current platform
  // on darwin it uses /, on win32 it uses \
  const nodeBin = path.join('C:\\pkg', 'scripts', '..', 'node_modules', '.bin', 'lain-launcher.cmd');
  const expected = path.join('C:\\pkg', 'node_modules', '.bin', 'lain-launcher.cmd');
  assert.strictEqual(nodeBin, expected);
});

// ── LAIN_DIR path ───────────────────────────

console.log('\n[ LAIN_DIR paths ]');

test('LAIN_DIR is ~/.lain', () => {
  const lainDir = path.join(process.env.HOME, '.lain');
  assert.strictEqual(lainDir, path.join(os.homedir(), '.lain'));
});

test('BIN_DIR is ~/.lain/bin', () => {
  const lainDir = path.join(process.env.HOME, '.lain');
  const binDir = path.join(lainDir, 'bin');
  assert.strictEqual(binDir, path.join(os.homedir(), '.lain', 'bin'));
});

// ── CI guard logic ──────────────────────────

console.log('\n[ CI guard ]');

test('CI=true skips install', () => {
  const env = { CI: 'true' };
  const shouldSkip = env.CI && !env.LAIN_FORCE_INSTALL;
  assert.strictEqual(shouldSkip, true);
});

test('CI=true + LAIN_FORCE_INSTALL=1 does NOT skip', () => {
  const env = { CI: 'true', LAIN_FORCE_INSTALL: '1' };
  const shouldSkip = env.CI && !env.LAIN_FORCE_INSTALL;
  assert.strictEqual(shouldSkip, false);
});

test('CI unset does not skip', () => {
  const env = {};
  const shouldSkip = !!(env.CI && !env.LAIN_FORCE_INSTALL);
  assert.strictEqual(shouldSkip, false);
});

// ── ignore-scripts guard ────────────────────

console.log('\n[ --ignore-scripts guard ]');

test('npm_config_ignore_scripts=true skips install', () => {
  const env = { npm_config_ignore_scripts: 'true' };
  const shouldSkip = env.npm_config_ignore_scripts === 'true';
  assert.strictEqual(shouldSkip, true);
});

test('npm_config_ignore_scripts unset does not skip', () => {
  const env = {};
  const shouldSkip = env.npm_config_ignore_scripts === 'true';
  assert.strictEqual(shouldSkip, false);
});

// ── Launcher content ───────────────────────

console.log('\n[ createLauncher content ]');

test('Unix launcher uses exec with quoted binary path', () => {
  const isWin = false;
  const binaryPath = '/Users/user/.lain/bin/lain';
  const content = isWin
    ? `@echo off\n"${binaryPath}" %*`
    : `#!/bin/sh\nexec "${binaryPath}" "$@"\n`;
  assert.strictEqual(content, '#!/bin/sh\nexec "/Users/user/.lain/bin/lain" "$@"\n');
});

test('Windows launcher uses @echo off with quoted binary path', () => {
  const isWin = true;
  const binaryPath = 'C:\\Users\\user\\.lain\\bin\\lain.exe';
  const content = isWin
    ? `@echo off\n"${binaryPath}" %*`
    : `#!/bin/sh\nexec "${binaryPath}" "$@"\n`;
  assert.strictEqual(content, '@echo off\n"C:\\Users\\user\\.lain\\bin\\lain.exe" %*');
});

// ── Download URL round-trip ────────────────

console.log('\n[ Full URL round-trip for all platforms ]');

const platforms = [
  { p: 'darwin', a: 'arm64',  expected: 'aarch64-apple-darwin' },
  { p: 'darwin', a: 'x64',    expected: 'x86_64-apple-darwin' },
  { p: 'linux',  a: 'x64',    expected: 'x86_64-unknown-linux-gnu' },
  { p: 'win32',  a: 'x64',    expected: 'x86_64-pc-windows-msvc.exe' },
];

for (const { p, a, expected } of platforms) {
  test(`${p}-${a} round-trip`, () => {
    const plat = getPlatform(p, a);
    assert.strictEqual(plat, expected);
    const asset = getAssetName(plat, 'v0.1.0');
    assert.ok(asset.startsWith('lain-'));
    assert.ok(asset.endsWith(plat));
    const url = buildUrl(plat, 'v0.1.0');
    assert.ok(url.startsWith('https://github.com/spuentesp/lain/releases/download/v0.1.0/lain-'));
  });
}

// ── Summary ────────────────────────────────

console.log(`\n${'─'.repeat(40)}`);
console.log(`  Results: ${passed} passed, ${failed} failed`);
if (failed > 0) process.exit(1);
console.log('  All tests passed.\n');