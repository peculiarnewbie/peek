#!/usr/bin/env node
// Builds a nightly of peek for every platform, publishes it as the `nightly`
// GitHub prerelease, and deploys the hosted client's nightly site,
// https://next.peek.peculiarnewbie.com.
//
// Run from Windows, from a commit already pushed, with what scripts/build.mjs
// lists installed, `gh` signed in, and the cf profile that hosts the site.
//
//   node scripts/nightly.mjs              build, publish and deploy
//   node scripts/nightly.mjs --dry-run    build into dist/nightly/ only; uploads nothing
//   node scripts/nightly.mjs --skip-build publish binaries already in target/
//   node scripts/nightly.mjs --cf-profile <name>   default: peculiarnewbie
//
// What it publishes is what `peek update --channel next` reads: one rolling
// release whose tag moves to each nightly's commit, with these assets:
//   peek-win32-x64.exe, peek-linux-x64, peek-linux-arm64
//   SHA256SUMS     "<sha256>  <asset>" per binary
//   version.json   {"version", "commit", "date", "channel"}
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { buildAll, commit, dirty, fail as stop, root, run, version } from './build.mjs';

const args = process.argv.slice(2);
const flags = new Set(args);
const dryRun = flags.has('--dry-run');
const skipBuild = flags.has('--skip-build');
const at = args.indexOf('--cf-profile');
const cfProfile = at >= 0 && args[at + 1] ? args[at + 1] : 'peculiarnewbie';
const fail = (message) => stop(`nightly: ${message}`);
const TAG = 'nightly';

if (!dryRun && dirty()) fail('the working tree has changes; publish only what is committed');
if (!dryRun) {
  const pushed = execFileSync('git', ['branch', '-r', '--contains', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
  if (!pushed) fail(`push ${commit} first: the release points at it on GitHub`);
}

const date = new Date().toISOString().slice(0, 10).replaceAll('-', '');
const build = `next.${date}.${commit}`;
const full = `${version}-${build}`;
console.log(`peek ${full}`);

const env = { PEEK_CHANNEL: 'next', PEEK_BUILD: build };
const targets = buildAll({ skip: skipBuild, env });

const out = path.join(root, 'dist', 'nightly');
fs.rmSync(out, { recursive: true, force: true });
fs.mkdirSync(out, { recursive: true });
const assets = [];
const sums = [];
for (const t of targets) {
  const name = `peek-${t.key}${t.exe.endsWith('.exe') ? '.exe' : ''}`;
  const file = path.join(out, name);
  fs.copyFileSync(t.bin, file);
  sums.push(`${createHash('sha256').update(fs.readFileSync(file)).digest('hex')}  ${name}`);
  assets.push(file);
}
fs.writeFileSync(path.join(out, 'SHA256SUMS'), `${sums.join('\n')}\n`);
fs.writeFileSync(path.join(out, 'version.json'),
  `${JSON.stringify({ version: full, commit, date, channel: 'next' }, null, 2)}\n`);
assets.push(path.join(out, 'SHA256SUMS'), path.join(out, 'version.json'));

// The binaries must say which nightly they are, or `peek update` cannot tell.
const own = targets.find((t) => t.key === `${process.platform}-${process.arch}`);
if (own) {
  const said = execFileSync(own.bin, ['--version'], { encoding: 'utf8' }).trim();
  if (!said.endsWith(full)) fail(`${own.bin} says "${said}", not ${full}: was it built with PEEK_BUILD set?`);
}

if (dryRun) {
  console.log(`\nBuilt ${full} into ${path.relative(root, out)}. Nothing was uploaded.`);
  process.exit(0);
}

const notes = `Nightly build ${full}, from ${commit}. Not a release: install it with \`peek update --channel next\`, ` +
  'and open https://next.peek.peculiarnewbie.com for the matching hosted client.';
run('git', ['tag', '-f', TAG, 'HEAD']);
run('git', ['push', '-f', 'origin', `refs/tags/${TAG}`]);
let exists = true;
try {
  execFileSync('gh', ['release', 'view', TAG], { cwd: root, stdio: 'ignore' });
} catch {
  exists = false;
}
if (exists) {
  run('gh', ['release', 'edit', TAG, '--prerelease', '--title', `Nightly ${full}`, '--notes', notes]);
  run('gh', ['release', 'upload', TAG, '--clobber', ...assets]);
} else {
  run('gh', ['release', 'create', TAG, '--prerelease', '--title', `Nightly ${full}`, '--notes', notes, ...assets]);
}

// The hosted client's nightly site (client/cloudflare.config.ts).
run('npm', ['run', 'deploy:hosted', '--', '--profile', cfProfile], {
  cwd: path.join(root, 'client'),
  env: { ...process.env, PEEK_CHANNEL: 'next' },
});
console.log(`\nPublished ${full}: https://github.com/peculiarnewbie/peek/releases/tag/${TAG}`);
console.log('Hosted client: https://next.peek.peculiarnewbie.com');
