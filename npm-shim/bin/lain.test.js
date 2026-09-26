#!/usr/bin/env node

const assert = require('assert');
const fs = require('fs');
const path = require('path');

const source = fs.readFileSync(path.join(__dirname, 'lain.js'), 'utf8');
assert(source.includes("require('../scripts/runtime')"));
assert(source.includes('process.argv.slice(2)'));
assert(source.includes("stdio: 'inherit'"));
assert(!source.includes("'.lain'"), 'launcher must not hard-code the legacy shared cache');
assert(source.includes('Windows status'), 'launcher must surface Windows NTSTATUS exits instead of dying silently');
assert(source.includes('exitCode > 255') || source.includes('exitCode < 0 || exitCode > 255'), 'launcher must clamp out-of-range exit codes');
process.stdout.write('launcher delegates cache selection and forwards arguments\n');
