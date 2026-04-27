#!/usr/bin/env node
// Launcher stub — included in npm package.
// Real binary is downloaded by postinstall to ~/.lain/bin/

const path = require('path');
const { spawn } = require('child_process');
const fs = require('fs');

const LAIN_DIR = path.join(process.env.HOME, '.lain');
const launcherName = process.platform === 'win32' ? 'lain-launcher.cmd' : 'lain-launcher';
const launcherPath = path.join(LAIN_DIR, 'bin', launcherName);

if (!fs.existsSync(launcherPath)) {
  console.error(`Lain binary not found at ${launcherPath}`);
  console.error("If you ran 'npx', the binary downloads on first install.");
  console.error("Run: npm install -g lain-mcp  (or npx lain-mcp to trigger download)");
  process.exit(1);
}

const child = spawn(launcherPath, process.argv.slice(2), {
  stdio: 'inherit',
  cwd: process.cwd(),
});

child.on('exit', (code) => process.exit(code ?? 0));
child.on('error', (err) => {
  console.error('Failed to start lain:', err.message);
  process.exit(1);
});
