#!/usr/bin/env node

const assert = require('assert');
const fs = require('fs');
const path = require('path');

const source = fs.readFileSync(path.join(__dirname, 'lain.js'), 'utf8');
assert(source.includes("require('../scripts/runtime')"));
assert(source.includes('process.argv.slice(2)'));
assert(source.includes("stdio: 'inherit'"));
assert(!source.includes("'.lain'"), 'launcher must not hard-code the legacy shared cache');
process.stdout.write('launcher delegates cache selection and forwards arguments\n');
