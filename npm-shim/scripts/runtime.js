'use strict';

const crypto = require('crypto');
const fs = require('fs');
const https = require('https');
const os = require('os');
const path = require('path');
const { execFileSync, spawnSync } = require('child_process');

const REPOSITORY = 'spuentesp/lain';

function targetFor(platform = process.platform, arch = process.arch) {
  const targets = { 'darwin-arm64': 'aarch64-apple-darwin', 'linux-x64': 'x86_64-unknown-linux-gnu', 'win32-x64': 'x86_64-pc-windows-msvc' };
  const target = targets[`${platform}-${arch}`];
  if (!target) throw new Error(`Unsupported platform: ${platform}-${arch}`);
  return target;
}

function normalizeVersion(value) {
  const version = String(value || '').replace(/^v/, '');
  if (!/^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$/.test(version)) throw new Error(`Invalid LAIN_VERSION: ${value}`);
  return version;
}

function selectedVersion(env = process.env) {
  return normalizeVersion(env.LAIN_VERSION || require('../package.json').version);
}

function cacheRoot(env = process.env, platform = process.platform) {
  if (env.LAIN_CACHE_DIR) return path.resolve(env.LAIN_CACHE_DIR);
  if (platform === 'win32') return path.join(env.LOCALAPPDATA || path.join(os.homedir(), 'AppData', 'Local'), 'lain', 'Cache');
  if (platform === 'darwin') return path.join(os.homedir(), 'Library', 'Caches', 'lain');
  return path.join(env.XDG_CACHE_HOME || path.join(os.homedir(), '.cache'), 'lain');
}

function assetName(version, target) { return `lain-${normalizeVersion(version)}-${target}.tar.gz`; }

function binaryPath(version, target, env = process.env, platform = process.platform) {
  return path.join(cacheRoot(env, platform), normalizeVersion(version), target, platform === 'win32' ? 'lain.exe' : 'lain');
}

function releaseUrl(version, name) { return `https://github.com/${REPOSITORY}/releases/download/v${normalizeVersion(version)}/${name}`; }

function parseChecksum(text, expectedAsset) {
  for (const line of text.split(/\r?\n/)) {
    const match = line.trim().match(/^([a-fA-F0-9]{64})\s+\*?(.+)$/);
    if (match && match[2] === expectedAsset) return match[1].toLowerCase();
  }
  throw new Error(`SHA256SUMS has no checksum for ${expectedAsset}`);
}

function sha256(file) {
  const hash = crypto.createHash('sha256');
  hash.update(fs.readFileSync(file));
  return hash.digest('hex');
}

function verifyArchive(file, expected) {
  const actual = sha256(file);
  if (actual !== expected.toLowerCase()) throw new Error(`Checksum mismatch for ${path.basename(file)}: expected ${expected}, got ${actual}`);
}

function verifyBinary(file, version) {
  const result = spawnSync(file, ['--version'], { encoding: 'utf8', timeout: 10_000 });
  if (result.error) throw new Error(`Cannot run downloaded binary: ${result.error.message}`);
  if (result.status !== 0) throw new Error(`Downloaded binary exited ${result.status} during version check`);
  const reported = String(result.stdout).trim();
  if (reported !== `lain ${normalizeVersion(version)}`) throw new Error(`Downloaded binary reports ${JSON.stringify(reported)}, expected "lain ${normalizeVersion(version)}"`);
}

function verifySidecarBinary(file, version) {
  const result = spawnSync(file, ['--version'], { encoding: 'utf8', timeout: 10_000 });
  if (result.error) throw new Error(`Cannot run downloaded sidecar: ${result.error.message}`);
  if (result.status !== 0) throw new Error(`Downloaded sidecar exited ${result.status} during version check`);
  const reported = String(result.stdout).trim();
  if (reported !== `lain-git-sidecar ${normalizeVersion(version)}`) throw new Error(`Downloaded sidecar reports ${JSON.stringify(reported)}, expected "lain-git-sidecar ${normalizeVersion(version)}"`);
}

function download(url, destination, redirects = 5) {
  return new Promise((resolve, reject) => {
    const request = https.get(url, { headers: { 'User-Agent': '@spuentesp/lain-mcp' } }, (response) => {
      if ([301, 302, 303, 307, 308].includes(response.statusCode) && response.headers.location) {
        response.resume();
        if (redirects === 0) return reject(new Error(`Too many redirects downloading ${url}`));
        return download(response.headers.location, destination, redirects - 1).then(resolve, reject);
      }
      if (response.statusCode !== 200) {
        response.resume();
        return reject(new Error(`HTTP ${response.statusCode} downloading ${url}`));
      }
      const output = fs.createWriteStream(destination, { flags: 'wx' });
      let received = 0;
      const total = Number(response.headers['content-length']) || 0;
      response.on('data', (chunk) => {
        received += chunk.length;
        if (process.stderr.isTTY && total) process.stderr.write(`\rDownloading ${path.basename(destination)} ${Math.floor(received * 100 / total)}%`);
      });
      response.pipe(output);
      output.on('finish', () => output.close(() => {
        if (process.stderr.isTTY && total) process.stderr.write('\n');
        resolve();
      }));
      output.on('error', reject);
      response.on('error', reject);
    });
    request.setTimeout(30_000, () => request.destroy(new Error(`Download timed out: ${url}`)));
    request.on('error', reject);
  });
}

async function ensureBinary(options = {}) {
  const env = options.env || process.env;
  const platform = options.platform || process.platform;
  const arch = options.arch || process.arch;
  const version = normalizeVersion(options.version || selectedVersion(env));
  const target = targetFor(platform, arch);
  const finalBinary = binaryPath(version, target, env, platform);
  const directory = path.dirname(finalBinary);
  const sidecarName = platform === 'win32' ? 'lain-git-sidecar.exe' : 'lain-git-sidecar';
  const finalSidecar = path.join(directory, sidecarName);
  if (fs.existsSync(finalBinary)) {
    verifyBinary(finalBinary, version);
    if (fs.existsSync(finalSidecar)) {
      verifySidecarBinary(finalSidecar, version);
    }
    return finalBinary;
  }
  fs.mkdirSync(directory, { recursive: true });
  const temporary = path.join(directory, `.install-${process.pid}-${crypto.randomBytes(6).toString('hex')}`);
  fs.mkdirSync(temporary);
  const asset = assetName(version, target);
  const archive = path.join(temporary, asset);
  const sums = path.join(temporary, 'SHA256SUMS');
  try {
    const fetch = options.download || download;
    await fetch(releaseUrl(version, 'SHA256SUMS'), sums);
    await fetch(releaseUrl(version, asset), archive);
    verifyArchive(archive, parseChecksum(fs.readFileSync(sums, 'utf8'), asset));
    execFileSync('tar', ['xzf', archive, '-C', temporary], { stdio: ['ignore', 'ignore', 'inherit'] });
    const extracted = path.join(temporary, platform === 'win32' ? 'lain.exe' : 'lain');
    if (!fs.existsSync(extracted)) throw new Error(`${asset} does not contain the Lain executable at its root`);
    if (platform !== 'win32') fs.chmodSync(extracted, 0o755);
    verifyBinary(extracted, version);
    const extractedSidecar = path.join(temporary, sidecarName);
    if (fs.existsSync(extractedSidecar)) {
      if (platform !== 'win32') fs.chmodSync(extractedSidecar, 0o755);
      verifySidecarBinary(extractedSidecar, version);
      try {
        fs.renameSync(extractedSidecar, finalSidecar);
      } catch (error) {
        if (!fs.existsSync(finalSidecar)) throw error;
        verifySidecarBinary(finalSidecar, version);
      }
    }
    try {
      fs.renameSync(extracted, finalBinary);
    } catch (error) {
      if (!fs.existsSync(finalBinary)) throw error;
      verifyBinary(finalBinary, version);
    }
    return finalBinary;
  } finally {
    fs.rmSync(temporary, { recursive: true, force: true });
  }
}

module.exports = { assetName, binaryPath, cacheRoot, ensureBinary, normalizeVersion, parseChecksum, releaseUrl, selectedVersion, sha256, targetFor, verifyArchive, verifyBinary, verifySidecarBinary };

