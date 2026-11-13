// Build a normal installable plugin. No service installation or live config changes.
// Usage: node experiments/screen-plugin/build.mjs [path/to/peek.exe]
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const binary = resolve(process.argv[2] || `${root}/target/release/peek${process.platform === 'win32' ? '.exe' : ''}`);
const built = spawnSync('bun', ['run', 'build:screen-plugin'], { cwd: `${root}/client`, stdio: 'inherit', windowsHide: true });
if (built.status !== 0) process.exit(built.status || 1);
const folder = `${root}/experiments/screen-plugin/package`;
mkdirSync(`${folder}/bin`, { recursive: true });
mkdirSync(`${folder}/page`, { recursive: true });
copyFileSync(binary, `${folder}/bin/peek-screen${process.platform === 'win32' ? '.exe' : ''}`);
copyFileSync(`${root}/client/dist-screen/index.html`, `${folder}/page/index.html`);
writeFileSync(`${folder}/peek-plugin.json`, JSON.stringify({
  manifest_version: 1,
  provider_protocol: 1,
  name: 'screen-experiment',
  description: 'Opt-in screen plugin viability test. Requires an interactive desktop host; keeps the built-in screen available.',
  run: ['bin/peek-screen', '--provider', 'screen', '--provider-page', 'page'],
}, null, 2) + '\n');
console.log(`Built ${folder}`);
console.log('Install on an isolated interactive host, not the installed SYSTEM service. See README.md.');
