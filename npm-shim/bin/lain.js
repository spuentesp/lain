#!/usr/bin/env node

const { spawn } = require('child_process');
const { ensureBinary } = require('../scripts/runtime');

ensureBinary().then((binary) => {
  const child = spawn(binary, process.argv.slice(2), { cwd: process.cwd(), stdio: 'inherit' });
  child.on('error', (error) => {
    process.stderr.write(`Failed to start Lain: ${error.message}\n`);
    process.exitCode = 1;
  });
  child.on('exit', (code, signal) => {
    if (signal) process.kill(process.pid, signal);
    else process.exitCode = code == null ? 1 : code;
  });
}).catch((error) => {
  process.stderr.write(`Failed to prepare Lain: ${error.message}\n`);
  process.exitCode = 1;
});
