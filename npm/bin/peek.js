#!/usr/bin/env node
// Runs the peek binary for this platform, from vendor/<platform>-<arch>/.
// The package carries all of them; this picks one and hands it the terminal.
'use strict';

const { spawn } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const builds = {
  'win32-x64': 'peek.exe',
  'linux-x64': 'peek',
  'linux-arm64': 'peek',
};

const key = `${process.platform}-${process.arch}`;
if (!builds[key]) {
  console.error(`peek has no build for ${key}. It runs on Windows x64, and on Linux x64 or arm64 under Wayland.`);
  process.exit(1);
}
const bin = path.join(__dirname, '..', 'vendor', key, builds[key]);

// npm does not keep the executable bit on files packed from Windows.
if (process.platform !== 'win32') {
  try {
    fs.accessSync(bin, fs.constants.X_OK);
  } catch {
    try {
      fs.chmodSync(bin, 0o755);
    } catch (e) {
      console.error(`peek: cannot make ${bin} executable (${e.code}). Run: chmod +x "${bin}"`);
      process.exit(1);
    }
  }
}

const child = spawn(bin, process.argv.slice(2), { stdio: 'inherit' });

// Ctrl+C reaches peek directly through the shared terminal. Stay alive until
// it exits, and pass on the signals a service manager sends.
process.on('SIGINT', () => {});
for (const signal of ['SIGTERM', 'SIGHUP']) {
  process.on(signal, () => child.kill(signal));
}

child.on('error', (e) => {
  console.error(`peek: cannot start ${bin}: ${e.message}`);
  process.exit(1);
});
child.on('exit', (code, signal) => {
  process.exit(signal ? 1 : (code ?? 1));
});
