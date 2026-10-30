#!/usr/bin/env node

const { spawnSync } = require('child_process');
const { ensureBinary } = require('./runtime');

// Semantic search needs the default embedding model. A failed model install
// does not fail the package install; `lain models install` retries it.
function installModel(binary) {
  if (process.env.LAIN_SKIP_MODEL_INSTALL === '1') {
    return;
  }
  process.stderr.write('Installing the semantic search model (LAIN_SKIP_MODEL_INSTALL=1 skips it)...\n');
  const result = spawnSync(binary, ['models', 'install'], { stdio: 'inherit' });
  if (result.status !== 0) {
    process.stderr.write('Model install failed. Retry later with: lain models install\n');
  }
}

if (process.env.CI && !process.env.LAIN_FORCE_INSTALL) {
  process.stderr.write('Skipping Lain download in CI; set LAIN_FORCE_INSTALL=1 to test installation.\n');
} else if (process.env.npm_config_ignore_scripts === 'true') {
  process.stderr.write('Skipping Lain download because npm scripts are disabled.\n');
} else {
  ensureBinary()
    .then((binary) => installModel(binary))
    .catch((error) => {
      process.stderr.write(`Lain installation failed: ${error.message}\n`);
      process.exitCode = 1;
    });
}
