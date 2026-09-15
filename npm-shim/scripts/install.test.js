#!/usr/bin/env node

const assert = require('assert');
const crypto = require('crypto');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { execFileSync } = require('child_process');
const runtime = require('./runtime');

const tests = [];
function test(name, run) { tests.push({ name, run }); }

test('maps only published targets', () => {
  assert.strictEqual(runtime.targetFor('linux', 'x64'), 'x86_64-unknown-linux-gnu');
  assert.strictEqual(runtime.targetFor('darwin', 'arm64'), 'aarch64-apple-darwin');
  assert.strictEqual(runtime.targetFor('win32', 'x64'), 'x86_64-pc-windows-msvc');
  assert.throws(() => runtime.targetFor('linux', 'arm64'), /Unsupported/);
  assert.throws(() => runtime.targetFor('darwin', 'x64'), /Unsupported/);
});

test('LAIN_VERSION overrides package metadata and is validated', () => {
  assert.strictEqual(runtime.selectedVersion({ LAIN_VERSION: 'v1.2.3-rc1' }), '1.2.3-rc1');
  assert.throws(() => runtime.selectedVersion({ LAIN_VERSION: '../latest' }), /Invalid/);
});

test('cache paths include version and target', () => {
  const value = runtime.binaryPath('1.2.3', 'x86_64-unknown-linux-gnu', { LAIN_CACHE_DIR: '/cache' }, 'linux');
  assert.strictEqual(value, path.join('/cache', '1.2.3', 'x86_64-unknown-linux-gnu', 'lain'));
});

test('release asset contract omits the tag prefix', () => {
  const name = runtime.assetName('v1.2.3', 'aarch64-apple-darwin');
  assert.strictEqual(name, 'lain-1.2.3-aarch64-apple-darwin.tar.gz');
  assert(runtime.releaseUrl('1.2.3', name).endsWith(`/v1.2.3/${name}`));
});

test('checksum parser requires an exact asset match', () => {
  const hash = 'a'.repeat(64);
  assert.strictEqual(runtime.parseChecksum(`${hash}  lain-1-linux.tar.gz\n`, 'lain-1-linux.tar.gz'), hash);
  assert.throws(() => runtime.parseChecksum(`${hash}  other.tar.gz\n`, 'lain-1-linux.tar.gz'), /no checksum/);
});

test('checksum mismatch fails closed', () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'lain-checksum-'));
  try {
    const file = path.join(directory, 'archive');
    fs.writeFileSync(file, 'actual bytes');
    assert.throws(() => runtime.verifyArchive(file, '0'.repeat(64)), /Checksum mismatch/);
  } finally { fs.rmSync(directory, { recursive: true, force: true }); }
});

if (process.platform !== 'win32') {
  test('verified install is atomic and a second invocation is offline', async () => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'lain-install-'));
    const source = path.join(directory, 'source');
    const cache = path.join(directory, 'cache');
    fs.mkdirSync(source);
    const executable = path.join(source, 'lain');
    fs.writeFileSync(executable, '#!/bin/sh\nprintf "lain 1.2.3\\n"\n');
    fs.chmodSync(executable, 0o755);
    const asset = runtime.assetName('1.2.3', 'x86_64-unknown-linux-gnu');
    const archive = path.join(source, asset);
    execFileSync('tar', ['czf', archive, '-C', source, 'lain']);
    const digest = crypto.createHash('sha256').update(fs.readFileSync(archive)).digest('hex');
    fs.writeFileSync(path.join(source, 'SHA256SUMS'), `${digest}  ${asset}\n`);
    let downloads = 0;
    const download = async (url, destination) => {
      downloads += 1;
      fs.copyFileSync(path.join(source, path.basename(url)), destination);
    };
    const options = { version: '1.2.3', platform: 'linux', arch: 'x64', env: { LAIN_CACHE_DIR: cache }, download };
    try {
      const installed = await runtime.ensureBinary(options);
      assert.strictEqual(downloads, 2);
      assert.strictEqual(fs.statSync(installed).mode & 0o777, 0o755);
      assert(!fs.readdirSync(path.dirname(installed)).some((name) => name.startsWith('.install-')));
      await runtime.ensureBinary({ ...options, download: async () => { throw new Error('network used'); } });
      assert.strictEqual(downloads, 2);
    } finally { fs.rmSync(directory, { recursive: true, force: true }); }
  });

  test('downloaded binary version must match the selected release', () => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'lain-version-'));
    try {
      const executable = path.join(directory, 'lain');
      fs.writeFileSync(executable, '#!/bin/sh\nprintf "lain 9.9.9\\n"\n');
      fs.chmodSync(executable, 0o755);
      assert.throws(() => runtime.verifyBinary(executable, '1.2.3'), /expected/);
    } finally { fs.rmSync(directory, { recursive: true, force: true }); }
  });
}

(async () => {
  let failed = 0;
  for (const { name, run } of tests) {
    try { await run(); process.stdout.write(`ok - ${name}\n`); }
    catch (error) { failed += 1; process.stderr.write(`not ok - ${name}: ${error.stack}\n`); }
  }
  if (failed) process.exitCode = 1;
})();
