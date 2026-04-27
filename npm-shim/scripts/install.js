#!/usr/bin/env node
// postinstall script — downloads the Lain binary and creates ~/.lain/ structure

const { execSync } = require('child_process');
const fs = require('fs');
const path = require('path');

const LAIN_DIR = path.join(process.env.HOME, '.lain');
const BIN_DIR = path.join(LAIN_DIR, 'bin');
const MODELS_DIR = path.join(LAIN_DIR, 'models');
const TOOLCHAINS_DIR = path.join(LAIN_DIR, 'toolchains');
const TUNING_FILE = path.join(LAIN_DIR, 'tuning.toml');

// Skip postinstall in CI unless explicitly requested
if (process.env.CI && !process.env.LAIN_FORCE_INSTALL) {
  console.log('Skipping Lain install in CI environment (set LAIN_FORCE_INSTALL=1 to override)');
  return;
}

// Respect npm's --ignore-scripts flag
if (process.env.npm_config_ignore_scripts === 'true') {
  console.log('Skipping Lain install (npm --ignore-scripts detected)');
  return;
}

// Detect platform
function getPlatform() {
  const platform = process.platform;
  const arch = process.arch;
  if (platform === 'darwin' && arch === 'arm64') return 'aarch64-apple-darwin';
  if (platform === 'darwin' && arch === 'x64') return 'x86_64-apple-darwin';
  if (platform === 'linux' && arch === 'x64') return 'x86_64-unknown-linux-gnu';
  if (platform === 'win32' && arch === 'x64') return 'x86_64-pc-windows-msvc.exe';
  throw new Error(`Unsupported platform: ${platform}-${arch}`);
}

function getAssetName(platform, version) {
  const versionSlug = version.replace(/^v/, '');
  const map = {
    'aarch64-apple-darwin': `lain-${versionSlug}-aarch64-apple-darwin`,
    'x86_64-apple-darwin': `lain-${versionSlug}-x86_64-apple-darwin`,
    'x86_64-unknown-linux-gnu': `lain-${versionSlug}-x86_64-unknown-linux-gnu`,
    'x86_64-pc-windows-msvc.exe': `lain-${versionSlug}-x86_64-pc-windows-msvc.exe`,
  };
  const asset = map[platform];
  if (!asset) throw new Error(`Unsupported platform for binary download: ${platform}`);
  return asset;
}

// Ensure directory exists
function ensureDir(dir) {
  if (!fs.existsSync(dir)) {
    fs.mkdirSync(dir, { recursive: true });
  }
}

// Download file with curl, fallback to PowerShell on Windows
function download(url, dest) {
  console.log(`  Downloading ${path.basename(dest)}...`);
  try {
    execSync(`curl -L "${url}" -o "${dest}" --progress-bar`, { stdio: 'inherit' });
  } catch (e) {
    if (process.platform === 'win32') {
      console.log('  curl not found, using PowerShell...');
      execSync(
        `powershell -Command "Invoke-WebRequest -Uri \\"${url}\\" -OutFile \\"${dest}\\""`,
        { stdio: 'inherit' }
      );
    } else {
      throw new Error(`curl not found and no fallback available for ${process.platform}`);
    }
  }
}

// Create default tuning.toml
function createTuningConfig() {
  const content = `# Lain tuning configuration — loaded from ~/.lain/tuning.toml
# All values are optional; omit a field to use the built-in default.

[semantic]
threshold = 0.1        # Semantic search cosine similarity floor
anchor_weight = 0.3   # Weight for anchor_score in hybrid ranking

[ingestion]
max_pattern_edges = 200
lsp_pool_size = 4
files_per_batch = 50
max_files_per_scan = 5000
cochange_commit_window = 100
cochange_min_pair_count = 2
cochange_max_commit_files = 100

[execution]
default_command_timeout_secs = 60
default_test_timeout_secs = 300
lsp_symbol_poll_timeout_secs = 2
lsp_symbol_poll_interval_ms = 50
`;
  fs.writeFileSync(TUNING_FILE, content);
  console.log(`  Created ${TUNING_FILE}`);
}

// Create toolchains/ README
function createToolchainsDir() {
  ensureDir(TOOLCHAINS_DIR);
  const readme = `# User toolchain overrides
# Drop .toml files here to override built-in toolchain profiles.
# See the built-in profiles in the Lain source at toolchains/
`;
  fs.writeFileSync(path.join(TOOLCHAINS_DIR, 'README.md'), readme);
  console.log(`  Created ${TOOLCHAINS_DIR}/`);
}

// Create models/ README
function createModelsDir() {
  ensureDir(MODELS_DIR);
  const readme = `# ONNX embedding models
# Place .onnx model files here for semantic search.
# Required files:
#   model.onnx     — the ONNX model
#   tokenizer.json — the tokenizer
#
# Download the default model:
#   ./scripts/install.sh --fast  # if using Lain's install script
#   or from https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2
`;
  fs.writeFileSync(path.join(MODELS_DIR, 'README.md'), readme);
  console.log(`  Created ${MODELS_DIR}/`);
}

// Create the launcher script (shell for unix, batch for windows)
function createLauncher(binaryPath) {
  const isWin = process.platform === 'win32';
  const launcherName = isWin ? 'lain-launcher.cmd' : 'lain-launcher';
  const launcherPath = path.join(BIN_DIR, launcherName);
  const content = isWin
    ? `@echo off\n"${binaryPath}" %*`
    : `#!/bin/sh\nexec "${binaryPath}" "$@"\n`;
  fs.writeFileSync(launcherPath, content);
  if (!isWin) fs.chmodSync(launcherPath, 0o755);
  console.log(`  Created ${launcherPath}`);
}

// Create symlink in node_modules/.bin for npm-installed binaries
function createSymlink() {
  const isWin = process.platform === 'win32';
  const launcherName = isWin ? 'lain-launcher.cmd' : 'lain-launcher';
  // node_modules/.bin is inside the package, not a sibling of node_modules
  const nodeBin = path.join(__dirname, '..', 'node_modules', '.bin', launcherName);
  try {
    const binDir = path.dirname(nodeBin);
    if (!fs.existsSync(binDir)) {
      fs.mkdirSync(binDir, { recursive: true });
    }
    if (isWin) {
      // nodeBin already ends in .cmd via launcherName, don't append again
      fs.writeFileSync(nodeBin, `@echo off\n"${path.join(BIN_DIR, launcherName)}" %*`);
    } else {
      if (fs.existsSync(nodeBin)) fs.unlinkSync(nodeBin);
      fs.symlinkSync(path.join(BIN_DIR, launcherName), nodeBin);
    }
    console.log(`  Linked to node_modules/.bin/${launcherName}`);
  } catch (e) {
    if (e.code !== 'ENOENT' && e.code !== 'EEXIST') {
      console.warn(`  Warning: could not create .bin symlink: ${e.message}`);
    }
  }
}

// Main install
async function install() {
  console.log('\n=== Lain installer ===\n');

  // Ensure directories
  ensureDir(BIN_DIR);
  ensureDir(LAIN_DIR);

  const platform = getPlatform();
  const version = 'v0.1.0';
  const assetName = getAssetName(platform, version);
  // On unix the asset IS the binary; on windows it's .exe
  const binaryName = process.platform === 'win32' ? 'lain.exe' : 'lain';
  // Download to a staging name first, then rename to avoid overwriting launcher
  const stagingPath = path.join(BIN_DIR, binaryName + '.new');
  const binaryPath = path.join(BIN_DIR, binaryName);
  const githubRepo = 'spuentesp/lain';
  const url = `https://github.com/${githubRepo}/releases/download/${version}/${assetName}`;

  console.log(`  Platform: ${platform}`);
  console.log(`  Binary:   ${binaryPath}`);

  // Download binary
  if (fs.existsSync(binaryPath)) {
    console.log('  Binary already exists — skipping download');
  } else {
    console.log(`  Downloading from GitHub releases...`);
    download(url, stagingPath);
    fs.renameSync(stagingPath, binaryPath);
    if (!process.platform.startsWith('win')) {
      fs.chmodSync(binaryPath, 0o755);
    }
    console.log(`  Installed ${binaryPath}`);
  }

  // Create config directories
  if (!fs.existsSync(TUNING_FILE)) {
    createTuningConfig();
  } else {
    console.log(`  tuning.toml already exists — skipping`);
  }

  if (!fs.existsSync(TOOLCHAINS_DIR)) {
    createToolchainsDir();
  } else {
    console.log(`  toolchains/ already exists — skipping`);
  }

  if (!fs.existsSync(MODELS_DIR)) {
    createModelsDir();
  } else {
    console.log(`  models/ already exists — skipping`);
  }

  // Create launcher
  createLauncher(binaryPath);

  // Symlink for npm
  createSymlink();

  console.log('\n=== Installation complete ===\n');
  console.log('  To use Lain:');
  console.log(`    ${binaryPath} --workspace /path/to/project`);
  console.log('\n  Or add to your PATH:');
  console.log(`    export PATH="$HOME/.lain/bin:$PATH"\n`);
}

install().catch((err) => {
  console.error('Installation failed:', err.message);
  process.exit(1);
});
