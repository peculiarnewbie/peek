// End-to-end plugin checks against two isolated host processes. No installed
// service, user configuration, desktop input, or public registry is changed.
// cargo build --release
// cargo build --release --manifest-path apps/files/Cargo.toml
// bun bench/plugins.ts
import { strict as assert } from 'node:assert';
import { mkdtemp, mkdir, readFile, writeFile, cp, copyFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join, dirname } from 'node:path';

const root = resolve(import.meta.dir, '..');
const exe = resolve(process.argv.slice(2).find(a => !a.startsWith('--')) || join(root, 'target/release', process.platform === 'win32' ? 'peek.exe' : 'peek'));
const temp = await mkdtemp(join(tmpdir(), 'peek-e2e-'));
assert.equal(dirname(temp), resolve(tmpdir()));
const hosts: Host[] = [];
const sockets: PageSocket[] = [];
const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';

async function until<T>(fn: () => Promise<T>, what: string): Promise<T> {
  let last: unknown;
  for (let i = 0; i < 150; i++) {
    try { const value = await fn(); if (value) return value; } catch (e) { last = e; }
    await Bun.sleep(50);
  }
  throw new Error(`Timed out: ${what}${last ? ` (${last})` : ''}`);
}
async function run(command: string[], cwd = root) {
  const p = Bun.spawn(command, { cwd, stdout: 'pipe', stderr: 'pipe' });
  const [code, out, err] = await Promise.all([p.exited, new Response(p.stdout).text(), new Response(p.stderr).text()]);
  assert.equal(code, 0, `${command[0]} failed: ${err}`);
  return out;
}
class Host {
  process!: ReturnType<typeof Bun.spawn>;
  config: string;
  port: number;
  token = '';
  constructor(readonly name: string, readonly viewOnly = false) {
    this.config = join(temp, name);
    const reservation = Bun.serve({ port: 0, fetch: () => new Response() });
    this.port = reservation.port!;
    reservation.stop(true);
  }
  get base() { return `http://127.0.0.1:${this.port}`; }
  async start() {
    this.process = Bun.spawn([exe, '--bind', '127.0.0.1', '--port', String(this.port), '--web', 'none', ...(this.viewOnly ? ['--view-only'] : [])], {
      env: { ...process.env, PEEK_CONFIG_DIR: this.config }, stdout: 'ignore', stderr: 'ignore',
    });
    await until(async () => (await fetch(`${this.base}/.well-known/peek`)).ok, `${this.name} startup`);
    this.token = (await readFile(join(this.config, 'token'), 'utf8')).trim();
  }
  async stop() { this.process.kill(); await this.process.exited; }
  async cli(...args: string[]) {
    const p = Bun.spawn([exe, '--port', String(this.port), 'plugin', ...args], {
      env: { ...process.env, PEEK_CONFIG_DIR: this.config }, stdout: 'pipe', stderr: 'pipe',
    });
    const [code, out, err] = await Promise.all([p.exited, new Response(p.stdout).text(), new Response(p.stderr).text()]);
    assert.equal(code, 0, `plugin ${args[0]} failed: ${err}`);
    return out;
  }
  async ready(name: string) {
    await until(async () => {
      const d = await (await fetch(`${this.base}/.well-known/peek`)).json() as any;
      return d.providers.some((p: any) => p.name === name && p.frame);
    }, `${name} descriptor`);
  }
  async ticket(name: string, token = this.token) {
    return fetch(`${this.base}/api/frame/${name}`, { method: 'POST', headers: { 'content-type': 'application/json', Origin: this.base }, body: JSON.stringify({ token }) });
  }
  async page(name: string, ticket?: string) {
    ticket ??= (await (await this.ticket(name)).json() as any).ticket;
    const page = new PageSocket(`${this.base.replace('http', 'ws')}/api/frame/${name}/ws?ticket=${ticket}`, this.base);
    sockets.push(page);
    await page.opened;
    return page;
  }
}
class PageSocket {
  ws: WebSocket;
  messages: any[] = [];
  binary: ArrayBuffer[] = [];
  opened: Promise<void>;
  closed: Promise<number>;
  constructor(url: string, origin: string) {
    this.ws = new WebSocket(url, { headers: { Origin: origin } } as any);
    this.ws.binaryType = 'arraybuffer';
    this.opened = new Promise((resolve, reject) => { this.ws.onopen = () => resolve(); this.ws.onerror = () => reject(new Error('socket error')); });
    this.closed = new Promise(resolve => { this.ws.onclose = e => resolve(e.code); });
    this.ws.onmessage = e => typeof e.data === 'string' ? this.messages.push(JSON.parse(e.data)) : this.binary.push(e.data);
  }
  send(msg: object) { this.ws.send(JSON.stringify(msg)); }
  async answer(msg: object, type: string) {
    this.messages.length = 0;
    this.send(msg);
    return until(async () => this.messages.find(m => m.t === type), type);
  }
  close() { this.ws.close(); }
}

try {
  const a = new Host('a'), b = new Host('b', true);
  hosts.push(a, b);
  await a.start(); await b.start();
  const da = await (await fetch(`${a.base}/.well-known/peek`)).json() as any;
  const db = await (await fetch(`${b.base}/.well-known/peek`)).json() as any;
  assert.notEqual(da.id, db.id);
  assert.equal((await a.ticket('plugins', 'wrong')).status, 403);
  assert.equal((await a.ticket('plugins', b.token)).status, 403);
  console.log('PASS: independent host identities and authentication');

  const manager = await a.page('plugins');
  const source = join(temp, 'notes-source');
  await cp(join(root, 'examples/provider/frame'), source, { recursive: true });
  // Exercise a packaged distribution path without publishing anything.
  await writeFile(join(source, 'package.json'), JSON.stringify({ name: 'peek-test-notes', version: '0.0.1', scripts: { prepack: "node -e \"require('fs').writeFileSync('should-not-run','bad')\"" } }));
  await run([npm, 'pack', '--ignore-scripts', '--pack-destination', temp], source);
  const archive = join(temp, 'peek-test-notes-0.0.1.tgz');
  const manifest = JSON.parse(await readFile(join(source, 'peek-plugin.json'), 'utf8'));
  const name = manifest.name as string;
  await a.cli('add', archive); await a.ready(name);
  assert.equal(await Bun.file(join(source, 'should-not-run')).exists(), false);
  console.log('PASS: npm tarball installs through the CLI and appears without a host restart');

  const oldTicket = (await (await a.ticket(name)).json() as any).ticket;
  const notes = await a.page(name, oldTicket);
  await until(async () => notes.messages.find(m => m.t === 'notes'), 'initial notes');
  const result = await notes.answer({ t: 'do', do: { note: 'hello from integration test' } }, 'notes');
  assert.equal(result.notes.at(-1).text, 'hello from integration test');
  await until(async () => notes.binary.length > 0, 'binary samples');
  const wrongProvider = await a.page('plugins', oldTicket);
  assert.equal(await wrongProvider.closed, 4403);
  const restarted = notes.closed;
  assert.equal((await manager.answer({ t: 'restart', name }, 'ok')).t, 'ok');
  assert.equal(await restarted, 1012);
  await a.ready(name);
  const reopened = await a.page(name, oldTicket);
  await until(async () => reopened.messages.find(m => m.t === 'notes'), 'replacement provider');
  console.log('PASS: page text/binary, provider-scoped tickets, restart closes stale sockets, and reconnect');

  const initial = await manager.answer({ t: 'list' }, 'list');
  const builtin = initial.items.find((item: any) => item.name === 'files');
  assert.equal(builtin.kind, "peek's own");
  assert.equal(builtin.on, false);
  assert.equal((await a.ticket('files')).status, 404);
  assert.equal((await fetch(`${a.base}/p/files/`)).status, 404);
  const shared = join(temp, 'shared'); await mkdir(shared);
  await writeFile(join(shared, 'R\u00e9sum\u00e9.txt'), 'download integrity');
  assert.equal((await manager.answer({ t: 'settings', name: 'files', values: { folders: [shared] } }, 'ok')).t, 'ok');
  assert.equal((await manager.answer({ t: 'on', name: 'files', on: true }, 'ok')).t, 'ok');
  await a.ready('files');
  assert.equal((await fetch(`${a.base}/p/files/`)).status, 200);
  const builtinPage = await a.page('files');
  const builtinRoots = await builtinPage.answer({ t: 'roots' }, 'roots');
  assert.equal(builtinRoots.roots.length, 1);
  const builtinListing = await builtinPage.answer({ t: 'list', root: builtinRoots.roots[0].id, path: '' }, 'list');
  assert.ok(builtinListing.entries.some((entry: any) => entry.name === 'R\u00e9sum\u00e9.txt'));
  assert.equal((await manager.answer({ t: 'on', name: 'files', on: false }, 'ok')).t, 'ok');
  assert.equal((await a.ticket('files')).status, 404);
  assert.equal((await fetch(`${a.base}/p/files/`)).status, 404);
  console.log('PASS: built-in files starts, shares a folder, and stops');

  assert.equal(initial.items.find((item: any) => item.name === 'processes').on, false);
  assert.equal((await a.ticket('processes')).status, 404);
  assert.equal((await manager.answer({ t: 'on', name: 'processes', on: true }, 'ok')).t, 'ok');
  await a.ready('processes');
  const victim = Bun.spawn([process.execPath, '-e', 'setTimeout(() => {}, 60000)'], { stdout: 'ignore', stderr: 'ignore' });
  const procPage = await a.page('processes');
  procPage.send({ t: 'watch' });
  const firstTick = await until(async () => procPage.messages.find(m => m.t === 'tick' && m.full), 'first processes tick');
  assert.ok(firstTick.cores >= 1 && firstTick.mem[1] > 0 && firstTick.ending === true);
  const victimRow = firstTick.add.find((p: any[]) => p[0] === victim.pid);
  assert.ok(victimRow, 'the spawned process is listed');
  await until(async () => procPage.messages.find(m => m.t === 'tick' && m.cpu != null), 'processor figures');
  const end = (start: string, ref: number) => ({ t: 'do', do: { t: 'end', pid: victim.pid, start, force: false, ref } });
  assert.equal((await procPage.answer(end('1', 1), 'error')).why, 'it has already ended');
  assert.equal((await procPage.answer(end(victimRow[1], 2), 'ended')).pid, victim.pid);
  await victim.exited;
  await until(async () => procPage.messages.find(m => m.t === 'tick' && m.gone.includes(victim.pid)), 'ended process gone');
  const procCard = await fetch(`${a.base}/api/provider/processes`, { method: 'POST', headers: { 'content-type': 'application/json', Origin: a.base }, body: JSON.stringify({ token: a.token, t: 'view', place: 'card' }) });
  assert.ok(Array.isArray((await procCard.json() as any).view.items));
  procPage.close();
  assert.equal((await manager.answer({ t: 'on', name: 'processes', on: false }, 'ok')).t, 'ok');
  assert.equal((await a.ticket('processes')).status, 404);
  console.log('PASS: built-in processes lists, ends only the process it was shown, answers its card, and stops');

  const filesSource = join(temp, 'files-source');
  await cp(join(root, 'apps/files/page'), join(filesSource, 'page'), { recursive: true });
  await copyFile(join(root, 'apps/files/peek-plugin.json'), join(filesSource, 'peek-plugin.json'));
  await mkdir(join(filesSource, 'bin'));
  const filesExe = process.platform === 'win32' ? 'peek-files.exe' : 'peek-files';
  await copyFile(join(root, 'apps/files/target/release', filesExe), join(filesSource, 'bin', filesExe));
  const filesManifest = JSON.parse(await readFile(join(filesSource, 'peek-plugin.json'), 'utf8'));
  filesManifest.name = 'explorer';
  filesManifest.version = '1';
  await writeFile(join(filesSource, 'peek-plugin.json'), JSON.stringify(filesManifest));
  const inspected = await manager.answer({ t: 'inspect', source: filesSource }, 'inspected');
  assert.equal((await manager.answer({ t: 'install', id: inspected.id }, 'ok')).name, 'explorer');
  assert.equal((await manager.answer({ t: 'settings', name: 'explorer', values: { folders: [shared] } }, 'ok')).t, 'ok');
  const dataFile = join(a.config, 'plugin-data', 'explorer', 'kept');
  await writeFile(dataFile, 'persistent data');
  filesManifest.version = '2';
  await writeFile(join(filesSource, 'peek-plugin.json'), JSON.stringify(filesManifest));
  const replacement = await manager.answer({ t: 'inspect', source: filesSource }, 'inspected');
  assert.equal((await manager.answer({ t: 'update', id: replacement.id }, 'ok')).name, 'explorer');
  assert.equal(await readFile(dataFile, 'utf8'), 'persistent data');
  await a.ready('explorer');
  const files = await a.page('explorer');
  const roots = await files.answer({ t: 'roots' }, 'roots');
  assert.equal(roots.roots.length, 1);
  const folder = roots.roots[0].id;
  const listing = await files.answer({ t: 'list', root: folder, path: '' }, 'list');
  assert.ok(listing.entries.some((f: any) => f.name === 'R\u00e9sum\u00e9.txt'));
  assert.equal((await files.answer({ t: 'get', root: folder, path: '../outside.txt' }, 'error')).t, 'error');
  const offered = await files.answer({ t: 'get', root: folder, path: 'R\u00e9sum\u00e9.txt', ref: 1 }, 'file');
  const download = await fetch(a.base + offered.url);
  assert.ok(download.headers.get('content-disposition')?.includes('filename*=UTF-8'));
  assert.equal(await download.text(), 'download integrity');
  assert.equal((await fetch(a.base + offered.url)).status, 404);
  assert.equal((await manager.answer({ t: 'remove', name: 'explorer' }, 'ok')).t, 'ok');
  assert.equal((await a.ticket('explorer')).status, 404);
  console.log('PASS: native file plugin installs over management socket, saves settings, lists and downloads verified files');

  // A page path must remain inside its root after following directory links.
  const outside = join(temp, 'outside');
  await mkdir(outside); await writeFile(join(outside, 'secret.txt'), 'not a page asset');
  const pageDir = join(a.config, 'plugins', name, 'page');
  if (process.platform === 'win32') await run(['cmd.exe', '/c', 'mklink', '/J', join(pageDir, 'escape'), outside]);
  else { const { symlink } = await import('node:fs/promises'); await symlink(outside, join(pageDir, 'escape')); }
  assert.equal((await fetch(`${a.base}/p/${name}/escape/secret.txt`)).status, 404);
  console.log('PASS: linked page directories cannot expose files outside the page root');

  await b.cli('add', archive); await b.ready(name);
  const readOnly = await b.page(name);
  assert.equal((await readOnly.answer({ t: 'do', do: { note: 'refuse this' } }, 'error')).why, 'this machine is view-only');
  const readOnlyManager = await b.page('plugins');
  assert.equal((await readOnlyManager.answer({ t: 'restart', name }, 'error')).t, 'error');
  await a.cli('remote-install', 'off');
  assert.equal((await manager.answer({ t: 'remove', name }, 'error')).t, 'error');
  assert.equal((await manager.answer({ t: 'on', name, on: false }, 'ok')).t, 'ok');
  assert.equal((await manager.answer({ t: 'on', name, on: true }, 'ok')).t, 'ok');
  await a.ready(name);
  console.log('PASS: view-only and remote-install policies');

  if (process.argv.includes('--browser')) {
    const screen = process.argv.includes('--screen');
    if (screen) { await a.cli('add', join(root, 'experiments/screen-plugin/package')); await a.ready('screen-experiment'); }
    const { verifyBrowser } = await import('./browser-client');
    await verifyBrowser(a, async () => { await a.stop(); await a.start(); await a.ready(name); if (screen) await a.ready('screen-experiment'); }, screen);
  }

  // A full restart loses tickets; a newly authorized ticket works again.
  manager.close(); reopened.close();
  await a.stop(); await a.start(); await a.ready(name);
  const expired = await a.page(name, oldTicket);
  assert.equal(await expired.closed, 4403);
  const fresh = await a.page(name);
  await until(async () => fresh.messages.find(m => m.t === 'notes'), 'fresh authorization');
  const newManager = await a.page('plugins');
  await a.cli('remote-install', 'on');
  assert.equal((await newManager.answer({ t: 'remove', name }, 'ok')).t, 'ok');
  assert.equal((await a.ticket(name)).status, 404);
  console.log('PASS: host restart, renewed authorization, and live removal');
  console.log('Two isolated processes on one physical machine; physical cross-machine coverage remains separate. Browser coverage runs only with --browser.');
} finally {
  for (const socket of sockets) socket.close();
  for (const host of hosts) { try { await host.stop(); } catch {} }
  // The resolved target is the unique temporary directory created above.
  for (let attempt = 0; ; attempt++) {
    try { await rm(temp, { recursive: true, force: true }); break; }
    catch (error) { if (attempt === 30) throw error; await Bun.sleep(100); }
  }
}
