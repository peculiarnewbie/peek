// A running peek updating itself, end to end, on this machine (Windows: the
// logon-task way, where the new version takes over the port; the service
// restarts through Windows instead and is not run here).
//
//   bun bench/self-update.ts <old peek> <new peek>
//
// Both are builds of this branch: the new one with a different version, e.g.
// PEEK_BUILD=selftest. The old one is "installed" in a temporary folder and
// started there, with its own config folder and port, and asked to update from
// a release served on loopback. Nothing outside the temporary folder is used,
// and only processes running from it are ended.
import { strict as assert } from 'node:assert';
import { createHash } from 'node:crypto';
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const [oldExe, newExe] = process.argv.slice(2).map(p => resolve(p));
if (!oldExe || !newExe) throw new Error('usage: bun bench/self-update.ts <old peek> <new peek>');
const win = process.platform === 'win32';
const asset = win ? 'peek-win32-x64.exe' : process.arch === 'arm64' ? 'peek-linux-arm64' : 'peek-linux-x64';

const temp = await mkdtemp(join(tmpdir(), 'peek-self-update-'));
const installed = join(temp, 'installed');
const release = join(temp, 'release');
const config = join(temp, 'config');
const target = join(installed, win ? 'peek.exe' : 'peek');
const port = 19400 + Math.floor(Math.random() * 400);
const base = `http://127.0.0.1:${port}`;

const versionOf = async (exe: string) => {
  const p = Bun.spawn([exe, '--version'], { stdout: 'pipe' });
  const out = (await new Response(p.stdout).text()).trim();
  assert.equal(await p.exited, 0);
  return out.replace(/^peek /, '');
};
const sleep = (ms: number) => new Promise(r => setTimeout(r, ms));
async function about(): Promise<any> {
  const r = await fetch(`${base}/.well-known/peek`, { signal: AbortSignal.timeout(2000) });
  return r.json();
}
/** Wait until the peek on the port says `version`. */
async function runs(version: string, within = 30_000) {
  const until = Date.now() + within;
  let last = '';
  while (Date.now() < until) {
    try { last = (await about()).version; if (last === version) return; } catch { last = '(not answering)'; }
    await sleep(300);
  }
  throw new Error(`expected peek ${version} on ${base}, last saw ${last}`);
}
async function update(body: object, headers: Record<string, string> = {}) {
  const r = await fetch(`${base}/api/update`, {
    method: 'POST', headers: { 'content-type': 'application/json', ...headers }, body: JSON.stringify(body),
    signal: AbortSignal.timeout(120_000),
  });
  return { status: r.status, body: await r.json() as any };
}
/** End every process running from the temporary folder, and only those. */
async function endOurs() {
  if (!win) { Bun.spawnSync(['pkill', '-f', temp]); return; }
  const script = `Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -like '${temp.replaceAll("'", "''")}*' } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force }`;
  Bun.spawnSync(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', script]);
}

let server: ReturnType<typeof Bun.serve> | undefined;
try {
  await mkdir(installed); await mkdir(release); await mkdir(config);
  const oldVersion = await versionOf(oldExe);
  const newVersion = await versionOf(newExe);
  assert.notEqual(oldVersion, newVersion, 'the two builds must say different versions');
  await copyFile(oldExe, target);
  await copyFile(newExe, join(release, asset));
  const sum = createHash('sha256').update(await readFile(join(release, asset))).digest('hex');
  await writeFile(join(release, 'SHA256SUMS'), `${sum}  ${asset}\n`);
  await writeFile(join(release, 'version.json'), JSON.stringify({ version: newVersion }));
  server = Bun.serve({ hostname: '127.0.0.1', port: 0, fetch: req => new Response(Bun.file(join(release, new URL(req.url).pathname.slice(1)))) });

  const env = { ...process.env, PEEK_CONFIG_DIR: config, PEEK_UPDATE_TARGET: target, PEEK_UPDATE_URL: `http://127.0.0.1:${server.port}/` };
  const flags = ['--bind', '127.0.0.1', '--port', String(port), '--web', 'none', '--no-token'];
  Bun.spawn([target, ...flags], { env, stdout: 'ignore', stderr: 'ignore' });
  await runs(oldVersion);
  console.log(`installed peek ${oldVersion} is running on ${base}`);

  // A page of another site may not ask.
  let r = await update({}, { origin: 'https://example.com' });
  assert.equal(r.status, 403, JSON.stringify(r.body));

  // Asked as a browser would (no token needed here: --no-token, loopback).
  let t = Date.now();
  r = await update({});
  assert.equal(r.status, 200, JSON.stringify(r.body));
  assert.deepEqual([r.body.t, r.body.from, r.body.to], ['updated', oldVersion, newVersion]);
  console.log(`updated in ${Date.now() - t} ms`);
  t = Date.now();
  await runs(newVersion);
  console.log(`back on ${newVersion} ${Date.now() - t} ms after the answer`);
  assert.equal(await versionOf(target), newVersion);
  assert.equal(await versionOf(join(installed, win ? 'peek.prev.exe' : 'peek.prev')), oldVersion);

  // Nothing newer: nothing changes, nothing restarts.
  r = await update({});
  assert.deepEqual([r.status, r.body.t, r.body.version], [200, 'current', newVersion]);
  await runs(newVersion, 2000);

  // A browser cannot pick the channel or go back; `peek update` on the machine can.
  r = await update({ rollback: true });
  assert.equal(r.body.t, 'current', 'rollback from a browser is ignored');
  const secret = (await readFile(join(config, 'local-secret'), 'utf8')).trim();
  r = await update({ rollback: true }, { authorization: `Bearer ${secret}` });
  assert.deepEqual([r.status, r.body.t, r.body.to], [200, 'updated', oldVersion]);
  await runs(oldVersion);
  console.log(`rolled back to ${oldVersion}`);

  // A view-only peek offers no update and takes no request.
  await endOurs();
  await sleep(500);
  Bun.spawn([target, ...flags, '--view-only'], { env, stdout: 'ignore', stderr: 'ignore' });
  await runs(oldVersion);
  assert.equal((await about()).update ?? null, null);
  r = await update({});
  assert.deepEqual([r.status, r.body.why], [403, 'this machine is view-only']);

  console.log('self-update scenarios passed');
} finally {
  await endOurs();
  server?.stop(true);
  await sleep(500);
  await rm(temp, { recursive: true, force: true }).catch(e => console.log(`left ${temp}: ${e.message}`));
}
