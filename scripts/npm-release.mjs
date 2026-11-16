#!/usr/bin/env node
// Builds peek for every platform the npm package carries, packs
// @peculiarnewbie/peek, and optionally stages it on npm.
//
// Run from Windows, with what scripts/build.mjs lists installed.
//
//   node scripts/npm-release.mjs              build and pack into dist/npm/; uploads nothing
//   node scripts/npm-release.mjs --stage      also `npm stage publish` (NPM_TOKEN, or npm login)
//   node scripts/npm-release.mjs --skip-build pack binaries already in target/
//
// A staged version is not installable until a maintainer approves it with 2FA,
// on npmjs.com or with `npm stage approve <id>`.

import fs from 'node:fs';
import path from 'node:path';
import { buildAll, commit, dirty, fail as stop, root, run, version } from './build.mjs';

const flags = new Set(process.argv.slice(2));
const stage = flags.has('--stage');
const skipBuild = flags.has('--skip-build');
const repo = 'https://github.com/peculiarnewbie/peek';
const fail = (message) => stop(`npm-release: ${message}`);

if (stage && dirty()) fail('the working tree has changes; stage only what is committed');
console.log(`@peculiarnewbie/peek ${version} from ${commit}`);
const targets = buildAll({ skip: skipBuild });

// dist/npm/package/ is the package as published.
const out = path.join(root, 'dist', 'npm');
const pkgDir = path.join(out, 'package');
fs.rmSync(out, { recursive: true, force: true });
fs.mkdirSync(pkgDir, { recursive: true });

const manifest = JSON.parse(fs.readFileSync(path.join(root, 'npm', 'package.json'), 'utf8'));
manifest.version = version;
fs.writeFileSync(path.join(pkgDir, 'package.json'), `${JSON.stringify(manifest, null, 2)}\n`);
fs.cpSync(path.join(root, 'npm', 'bin'), path.join(pkgDir, 'bin'), { recursive: true });
for (const t of targets) {
  fs.mkdirSync(path.join(pkgDir, 'vendor', t.key), { recursive: true });
  fs.copyFileSync(t.bin, path.join(pkgDir, 'vendor', t.key, t.exe));
}
for (const extra of ['LICENSE', 'LICENSE.md']) {
  if (fs.existsSync(path.join(root, extra))) fs.copyFileSync(path.join(root, extra), path.join(pkgDir, extra));
}

// The README, with links that work on npmjs.com: images from raw GitHub,
// other relative links to the file on GitHub.
const readme = fs
  .readFileSync(path.join(root, 'README.md'), 'utf8')
  .replace(/(!?\[[^\]]*\])\((?!https?:|#|mailto:)([^)\s]+)\)/g, (_, label, link) =>
    label.startsWith('!')
      ? `${label}(${repo.replace('github.com', 'raw.githubusercontent.com')}/${commit}/${link})`
      : `${label}(${repo}/blob/${commit}/${link})`,
  );
fs.writeFileSync(path.join(pkgDir, 'README.md'), readme);

run('npm', ['pack', '--pack-destination', out], { cwd: pkgDir });
const tarball = path.join(out, `peculiarnewbie-peek-${version}.tgz`);
if (!fs.existsSync(tarball)) fail(`npm pack did not write ${tarball}`);

if (!stage) {
  console.log(`\nPacked ${path.relative(root, tarball)}. Nothing was uploaded; add --stage to stage it on npm.`);
  process.exit(0);
}

// NPM_TOKEN, if set, is read by npm from the environment: it is not written
// to disk here.
const args = ['stage', 'publish', tarball, '--access', 'public'];
if (process.env.NPM_TOKEN) {
  const npmrc = path.join(out, '.npmrc');
  fs.writeFileSync(npmrc, '//registry.npmjs.org/:_authToken=${NPM_TOKEN}\n');
  args.push('--userconfig', npmrc);
}
run('npm', args);
console.log(`\nStaged @peculiarnewbie/peek@${version}. Approve it with 2FA on npmjs.com, or: npm stage list, then npm stage approve <id>`);
