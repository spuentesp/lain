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
    if (signal) {
      process.kill(process.pid, signal);
      return;
    }
    const exitCode = code == null ? 1 : code;
    if (exitCode < 0 || exitCode > 255) {
      // Windows NTSTATUS values arrive as large integers (a missing
      // runtime DLL gives 0xC0000135). Assigning one to
      // process.exitCode throws a RangeError during Node's exit path,
      // which kills the process silently with code 1 — report the real
      // status and clamp instead.
      const status = (exitCode >>> 0).toString(16);
      process.stderr.write(`Lain exited with Windows status 0x${status}; 0xc0000135 means a runtime DLL (DirectML.dll) is missing next to lain.exe\n`);
      process.exitCode = 1;
      return;
    }
    process.exitCode = exitCode;
  });
}).catch((error) => {
  process.stderr.write(`Failed to prepare Lain: ${error.message}\n`);
  process.exitCode = 1;
});
