#!/usr/bin/env node
// Builds peek for every platform the npm package carries, packs
// @peculiarnewbie/peek, and optionally stages it on npm.
//
// Run from Windows (the Windows build needs MSVC), with the Linux targets and
// cargo-zigbuild installed:
//   rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
//   pip install ziglang && cargo install cargo-zigbuild --locked
//
//   node scripts/npm-release.mjs              build and pack into dist/npm/; uploads nothing
//   node scripts/npm-release.mjs --stage      also `npm stage publish` (NPM_TOKEN, or npm login)
//   node scripts/npm-release.mjs --skip-build pack binaries already in target/
//
// A staged version is not installable until a maintainer approves it with 2FA,
// on npmjs.com or with `npm stage approve <id>`.

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const flags = new Set(process.argv.slice(2));
const stage = flags.has('--stage');
const skipBuild = flags.has('--skip-build');
const repo = 'https://github.com/peculiarnewbie/peek';

// glibc 2.28 (2018) is the oldest these Linux builds will load on.
const targets = [
  { key: 'win32-x64', triple: 'x86_64-pc-windows-msvc', exe: 'peek.exe', cargo: ['build'] },
  { key: 'linux-x64', triple: 'x86_64-unknown-linux-gnu', exe: 'peek', cargo: ['zigbuild'], glibc: '2.28' },
  { key: 'linux-arm64', triple: 'aarch64-unknown-linux-gnu', exe: 'peek', cargo: ['zigbuild'], glibc: '2.28' },
];

function run(cmd, args, opts = {}) {
  console.log(`> ${cmd} ${args.join(' ')}`);
  // npm is a .cmd on Windows, which Node only starts through a shell.
  const shell = process.platform === 'win32' && cmd === 'npm';
  const quoted = shell ? args.map((a) => (/[\s"&|<>^]/.test(a) ? `"${a}"` : a)) : args;
  return execFileSync(cmd, quoted, { cwd: root, stdio: 'inherit', shell, ...opts });
}

function fail(message) {
  console.error(`npm-release: ${message}`);
  process.exit(1);
}

const version = fs.readFileSync(path.join(root, 'Cargo.toml'), 'utf8').match(/^version\s*=\s*"([^"]+)"/m)?.[1];
if (!version) fail('no version in Cargo.toml');

if (stage) {
  const dirty = execFileSync('git', ['status', '--porcelain'], { cwd: root, encoding: 'utf8' }).trim();
  if (dirty) fail('the working tree has changes; stage only what is committed');
}
const commit = execFileSync('git', ['rev-parse', '--short', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
console.log(`@peculiarnewbie/peek ${version} from ${commit}`);

// cargo-zigbuild looks for `zig`, then `python3 -m ziglang`; on Windows
// `python3` is often the Microsoft Store stub. Put pip's zig on PATH instead.
function findZig() {
  try {
    execFileSync('zig', ['version'], { stdio: 'ignore' });
    return;
  } catch {}
  for (const python of ['python', 'python3', 'py']) {
    try {
      const dir = execFileSync(python, ['-c', 'import ziglang, os; print(os.path.dirname(ziglang.__file__))'], {
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'ignore'],
      }).trim();
      if (dir) {
        process.env.PATH = `${dir}${path.delimiter}${process.env.PATH}`;
        return;
      }
    } catch {}
  }
  fail('Zig not found: pip install ziglang (or put zig on PATH)');
}
if (!skipBuild) findZig();

for (const t of targets) {
  if (!skipBuild) {
    const target = t.glibc ? `${t.triple}.${t.glibc}` : t.triple;
    run('cargo', [...t.cargo, '--release', '--locked', '--target', target]);
  }
  t.bin = path.join(root, 'target', t.triple, 'release', t.exe);
  if (!fs.existsSync(t.bin)) fail(`missing ${t.bin}; build it, or drop --skip-build`);
}

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
