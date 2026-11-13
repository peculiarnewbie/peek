// Run after cargo build -j 4 --release.
import { strict as assert } from 'node:assert';
import { createHash } from 'node:crypto';
import { mkdtemp, mkdir, copyFile, readFile, writeFile, rm, readdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const temp = await mkdtemp(join(tmpdir(), 'peek-update-test-'));
const exe = resolve('target/release', process.platform === 'win32' ? 'peek.exe' : 'peek');
const name = process.platform === 'win32' ? 'peek-win32-x64.exe' : process.arch === 'arm64' ? 'peek-linux-arm64' : 'peek-linux-x64';
const installed = join(temp, 'installed');
const release = join(temp, 'release');
const target = join(installed, process.platform === 'win32' ? 'peek.exe' : 'peek');
const prev = join(installed, process.platform === 'win32' ? 'peek.prev.exe' : 'peek.prev');
const asset = join(release, name);
let server: ReturnType<typeof Bun.serve> | undefined;
const digest = async (p: string) => createHash('sha256').update(await readFile(p)).digest('hex');
async function run(args: string[], url?: string) {
  const p = Bun.spawn([exe, 'update', ...args], { env: { ...process.env, PEEK_UPDATE_TARGET: target, PEEK_UPDATE_URL: url ?? `http://127.0.0.1:${server!.port}/` }, stdout: 'pipe', stderr: 'pipe' });
  const [code, out, err] = await Promise.all([p.exited, new Response(p.stdout).text(), new Response(p.stderr).text()]);
  return { code, text: out + err };
}
async function snapshot() {
  const files = (await readdir(installed)).sort();
  return Promise.all(files.map(async f => [f, await digest(join(installed, f))]));
}
try {
  await mkdir(installed); await mkdir(release);
  await copyFile(exe, target); await copyFile(exe, asset);
  const p = Bun.spawn([exe, '--version'], { stdout: 'pipe' });
  const output = (await new Response(p.stdout).text()).trim(); assert.equal(await p.exited, 0);
  assert.ok(output.startsWith('peek ')); const version = output.slice(5);
  const sum = await digest(asset);
  const sums = async (hash: string) => writeFile(join(release, 'SHA256SUMS'), `${hash}  ${name}\n`);
  const versionFile = async (v: string) => writeFile(join(release, 'version.json'), JSON.stringify({ version: v }));
  await sums(sum); await versionFile(version);
  server = Bun.serve({ hostname: '127.0.0.1', port: 0, fetch(req) { const file = Bun.file(join(release, new URL(req.url).pathname.slice(1))); return new Response(file); } });
  const before = await snapshot();
  let result = await run([]); assert.equal(result.code, 0, result.text); assert.match(result.text, /already the latest/); assert.deepEqual(await snapshot(), before);
  result = await run(['--check']); assert.equal(result.code, 0, result.text); assert.ok(result.text.includes(`installed: ${version};`)); assert.ok(result.text.includes(`: ${version}`));
  await writeFile(target, 'old');
  result = await run([]); assert.equal(result.code, 0, result.text); assert.equal(await digest(target), sum); assert.equal(await readFile(prev, 'utf8'), 'old');
  result = await run(['--rollback']); assert.equal(result.code, 0, result.text); assert.equal(await readFile(target, 'utf8'), 'old');
  result = await run(['--rollback']); assert.equal(result.code, 0, result.text); assert.equal(await digest(target), sum);
  await sums('0'.repeat(64)); const saved = await snapshot();
  result = await run([]); assert.notEqual(result.code, 0); assert.match(result.text, /the download does not match SHA256SUMS; nothing was changed/); assert.deepEqual(await snapshot(), saved);
  await sums(sum); await versionFile('0.0.0-wrong');
  result = await run([]); assert.notEqual(result.code, 0); assert.match(result.text, /the downloaded peek says .* not 0\.0\.0-wrong; nothing was changed/); assert.deepEqual(await snapshot(), saved);
  result = await run([], 'http://example.com/'); assert.notEqual(result.code, 0); assert.match(result.text, /update address/); assert.deepEqual(await snapshot(), saved);
  console.log('update end-to-end scenarios passed');
} finally { server?.stop(true); await rm(temp, { recursive: true, force: true }); }
