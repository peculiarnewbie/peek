// What the release scripts share: the platforms peek ships for, and building
// them all from Windows (npm-release.mjs, nightly.mjs).
//
// Needs, besides Rust with MSVC: the Linux targets and cargo-zigbuild.
//   rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
//   pip install ziglang && cargo install cargo-zigbuild --locked
import { execFileSync, execSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

// glibc 2.28 (2018) is the oldest these Linux builds will load on.
export const targets = [
  { key: 'win32-x64', triple: 'x86_64-pc-windows-msvc', exe: 'peek.exe', cargo: ['build'] },
  { key: 'linux-x64', triple: 'x86_64-unknown-linux-gnu', exe: 'peek', cargo: ['zigbuild'], glibc: '2.28' },
  { key: 'linux-arm64', triple: 'aarch64-unknown-linux-gnu', exe: 'peek', cargo: ['zigbuild'], glibc: '2.28' },
];

export function run(cmd, args, opts = {}) {
  console.log(`> ${cmd} ${args.join(' ')}`);
  // npm is a .cmd on Windows, which Node only starts through a shell: hand
  // it one command line, quoted here, rather than arguments to concatenate.
  if (process.platform === 'win32' && cmd === 'npm') {
    const line = ['npm', ...args.map((a) => (/[\s"&|<>^%]/.test(a) ? `"${a.replace(/"/g, '""')}"` : a))].join(' ');
    return execSync(line, { cwd: root, stdio: 'inherit', ...opts });
  }
  return execFileSync(cmd, args, { cwd: root, stdio: 'inherit', ...opts });
}

export function fail(message) {
  console.error(message);
  process.exit(1);
}

export const version = fs.readFileSync(path.join(root, 'Cargo.toml'), 'utf8').match(/^version\s*=\s*"([^"]+)"/m)?.[1];
if (!version) fail('no version in Cargo.toml');

export const commit = execFileSync('git', ['rev-parse', '--short', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();

export function dirty() {
  return execFileSync('git', ['status', '--porcelain'], { cwd: root, encoding: 'utf8' }).trim() !== '';
}

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

/** Builds every target (unless `skip`), with `env` added, and sets each one's `bin`. */
export function buildAll({ skip = false, env = {} } = {}) {
  if (!skip) findZig();
  for (const t of targets) {
    if (!skip) {
      const target = t.glibc ? `${t.triple}.${t.glibc}` : t.triple;
      run('cargo', [...t.cargo, '--release', '--locked', '--target', target], { env: { ...process.env, ...env } });
    }
    t.bin = path.join(root, 'target', t.triple, 'release', t.exe);
    if (!fs.existsSync(t.bin)) fail(`missing ${t.bin}; build it, or drop --skip-build`);
  }
  return targets;
}
