#!/usr/bin/env node
// Publishes a stable release on GitHub, v<version>, as the latest release:
// what `peek update` and the client's Update button install on the stable
// channel (https://github.com/peculiarnewbie/peek/releases/latest/download/).
//
// Run from Windows, from the release commit already pushed, with `gh` signed
// in. Publish the same binaries as the npm package, after it is approved:
//
//   node scripts/npm-release.mjs --stage          build, pack and stage on npm
//   (approve the staged package with 2FA)
//   node scripts/github-release.mjs --skip-build  publish those binaries here
//
//   node scripts/github-release.mjs --dry-run     write dist/release/ only
//
// The assets are the nightly's (scripts/nightly.mjs): peek-win32-x64.exe,
// peek-linux-x64, peek-linux-arm64, SHA256SUMS and version.json.
import { execFileSync } from 'node:child_process';
import path from 'node:path';
import { buildAll, checkVersion, commit, dirty, fail as stop, root, run, version, writeRelease } from './build.mjs';

const flags = new Set(process.argv.slice(2));
const dryRun = flags.has('--dry-run');
const skipBuild = flags.has('--skip-build');
const fail = (message) => stop(`github-release: ${message}`);
const tag = `v${version}`;

if (!dryRun && dirty()) fail('the working tree has changes; publish only what is committed');
if (!dryRun) {
  const pushed = execFileSync('git', ['branch', '-r', '--contains', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
  if (!pushed) fail(`push ${commit} first: the release points at it on GitHub`);
  let exists = true;
  try {
    execFileSync('gh', ['release', 'view', tag], { cwd: root, stdio: 'ignore' });
  } catch {
    exists = false;
  }
  if (exists) fail(`${tag} is published already; a release is never replaced`);
}

// A stable build: no PEEK_CHANNEL or PEEK_BUILD, so it says just the version.
delete process.env.PEEK_CHANNEL;
delete process.env.PEEK_BUILD;
const targets = buildAll({ skip: skipBuild });
checkVersion(targets, version);
const date = new Date().toISOString().slice(0, 10).replaceAll('-', '');
const out = path.join(root, 'dist', 'release');
const assets = writeRelease(targets, out, { version, commit, date, channel: 'stable' });

if (dryRun) {
  console.log(`\nWrote ${tag} into ${path.relative(root, out)}. Nothing was uploaded.`);
  process.exit(0);
}

const notes = `peek ${version}. What changed: https://peek.peculiarnewbie.com/changelog/#${version}\n\n` +
  'Install with `npm install -g @peculiarnewbie/peek`, then `peek install`. ' +
  'A peek that is installed already updates with `peek update`, or Update in the client.';
run('git', ['tag', tag, 'HEAD']);
run('git', ['push', 'origin', `refs/tags/${tag}`]);
run('gh', ['release', 'create', tag, '--latest', '--title', `peek ${version}`, '--notes', notes, ...assets]);
console.log(`\nPublished ${tag}: https://github.com/peculiarnewbie/peek/releases/tag/${tag}`);
