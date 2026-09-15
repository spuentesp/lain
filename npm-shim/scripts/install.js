#!/usr/bin/env node

const { ensureBinary } = require('./runtime');

if (process.env.CI && !process.env.LAIN_FORCE_INSTALL) {
  process.stderr.write('Skipping Lain download in CI; set LAIN_FORCE_INSTALL=1 to test installation.\n');
} else if (process.env.npm_config_ignore_scripts === 'true') {
  process.stderr.write('Skipping Lain download because npm scripts are disabled.\n');
} else {
  ensureBinary().catch((error) => {
    process.stderr.write(`Lain installation failed: ${error.message}\n`);
    process.exitCode = 1;
  });
}
