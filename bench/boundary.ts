// SPIKE: what putting the screen behind a process boundary costs
// (docs/extensibility.md). Starts this checkout's release build twice over,
// as one process and with `--split`, on a port of its own, and compares:
//
//   bun bench/boundary.ts [--rounds 2] [--frames 200] [--cold] [--fake 2560x1440]
//
// - first frame: connect, wait for the whole first frame, close. Warm (the
//   capture still set up); with --cold also from idle, 6 s apart.
// - full frames back to back: after each frame the quality setting is
//   switched, which makes the host send the whole screen again at once. The
//   round trip less the host's own capture-to-send time is the travelling,
//   the part a boundary adds to.
// - engine to socket: the host's own timing of each frame, from the engine
//   handing it over to it being written to the socket (PEEK_HOPS).
//
// The modes take turns, so drift on the machine falls on both. --fake shows
// the host a made-up picture instead of the desktop, for running where there
// is no display. Nothing is injected: the hosts run with --view-only, and
// keep their settings under target/spike, away from an installed peek's.
import { mkdirSync, readFileSync, rmSync, existsSync } from 'node:fs';
import { join } from 'node:path';

/** Must match PROTOCOL in src/server.rs. */
const PROTOCOL = 4;
const PORT = 9192;
const URL_WS = `ws://127.0.0.1:${PORT}/ws`, ORIGIN = `http://127.0.0.1:${PORT}`;

const argv = process.argv.slice(2);
const flag = (name: string) => argv.includes(name);
const value = (name: string, fallback: string) => { const i = argv.indexOf(name); return i >= 0 ? argv[i + 1] : fallback; };
const rounds = Number(value('--rounds', '2')), frames = Number(value('--frames', '200'));
const cold = flag('--cold'), fake = value('--fake', '');

const root = join(import.meta.dir, '..');
const exe = join(root, 'target', 'release', process.platform === 'win32' ? 'peek.exe' : 'peek');
const scratch = join(root, 'target', 'spike');
const config = join(scratch, 'config');
mkdirSync(config, { recursive: true });

const MODES = ['one process', 'split'] as const;
type Mode = typeof MODES[number];
interface Samples { warm: number[]; cold: number[]; trip: number[]; travel: number[]; host: number[]; kb: number[]; hop: number[]; vitals: number }
const blank = (): Samples => ({ warm: [], cold: [], trip: [], travel: [], host: [], kb: [], hop: [], vitals: 0 });
const got: Record<Mode, Samples> = { 'one process': blank(), split: blank() };
let dims = '', lastVitals = '';

const query = (token: string) => new URLSearchParams({
  v: String(PROTOCOL), token, q: 'med', scale: '1', fps: '60', soft: '0', lossless: '0', mon: '0', vis: '1' });
const open = (token: string) => {
  const ws = new WebSocket(`${URL_WS}?${query(token)}`, { headers: { Origin: ORIGIN } } as any);
  ws.binaryType = 'arraybuffer';
  return ws;
};

/** Milliseconds from opening a socket to its whole first frame. */
function firstFrame(token: string): Promise<number> {
  return new Promise((resolve, reject) => {
    const t0 = performance.now();
    const ws = open(token);
    const timer = setTimeout(() => { ws.close(); reject(new Error('no frame within 10 s (no display here? try --fake 2560x1440)')); }, 10000);
    ws.onmessage = e => {
      if (typeof e.data === 'string') return;
      const ms = performance.now() - t0;
      clearTimeout(timer);
      ws.close();
      resolve(ms);
    };
    ws.onclose = e => { if (e.code >= 4000) { clearTimeout(timer); reject(new Error(`refused (${e.code} ${e.reason})`)); } };
  });
}

/** `frames` full frames back to back on one socket. */
function stream(token: string, into: Samples): Promise<void> {
  return new Promise((resolve, reject) => {
    const ws = open(token);
    const timer = setTimeout(() => { ws.close(); reject(new Error('the stream stalled')); }, 60000);
    let asked = 0, quality = 'med', count = 0;
    ws.onmessage = e => {
      if (typeof e.data === 'string') {
        if (JSON.parse(e.data).t === 'vitals') { into.vitals++; lastVitals = e.data; }
        return;
      }
      const now = performance.now();
      const dv = new DataView(e.data as ArrayBuffer);
      const seq = dv.getUint32(1, true), hostMs = dv.getUint32(11, true) / 1000;
      dims = `${dv.getUint16(5, true)}×${dv.getUint16(7, true)}`;
      // The first frame answers the connection, not a question from here.
      if (asked) {
        into.trip.push(now - asked);
        into.travel.push(now - asked - hostMs);
        into.host.push(hostMs);
        into.kb.push((e.data as ArrayBuffer).byteLength / 1024);
        count++;
      }
      if (count >= frames) { clearTimeout(timer); ws.close(); resolve(); return; }
      // The setting first, then the ack that makes room for a frame, so the
      // host sends the whole screen as soon as the ack arrives.
      quality = quality === 'med' ? 'high' : 'med';
      setTimeout(() => {
        asked = performance.now();
        ws.send(JSON.stringify({ t: 'cfg', q: quality }));
        ws.send(JSON.stringify({ t: 'ack', s: seq }));
      }, 5);
    };
  });
}

async function run(mode: Mode) {
  const hops = join(scratch, 'hops.csv');
  rmSync(hops, { force: true });
  const env: Record<string, string> = { ...process.env as Record<string, string>, PEEK_HOPS: hops };
  env[process.platform === 'win32' ? 'APPDATA' : 'XDG_CONFIG_HOME'] = config;
  if (fake) env.PEEK_FAKE_SCREEN = fake;
  const args = ['--bind', '127.0.0.1', '--port', String(PORT), '--web', 'none', '--view-only'];
  if (mode === 'split') args.push('--split');
  const host = Bun.spawn([exe, ...args], { env, stdout: 'ignore', stderr: Bun.file(join(scratch, `${mode.replace(' ', '-')}.log`)) });
  try {
    for (let i = 0; ; i++) {
      if (await fetch(`${ORIGIN}/.well-known/peek`).then(r => r.ok, () => false)) break;
      if (i > 50) throw new Error(`peek did not start; see ${scratch}`);
      await Bun.sleep(100);
    }
    const token = readFileSync(join(config, 'peek', 'token'), 'utf8').trim();
    const into = got[mode];
    await firstFrame(token);   // sets the capture up
    for (let i = 0; i < 20; i++) { await Bun.sleep(200); into.warm.push(await firstFrame(token)); }
    if (cold) for (let i = 0; i < 8; i++) { await Bun.sleep(6000); into.cold.push(await firstFrame(token)); }
    await stream(token, into);
    await Bun.sleep(300);   // the host writes its timings as the socket closes
    if (existsSync(hops)) {
      for (const line of readFileSync(hops, 'utf8').split('\n')) if (line) into.hop.push(Number(line.split(',')[0]) / 1000);
    }
  } finally {
    host.kill();
    await host.exited;
    await Bun.sleep(300);   // and its providers, once their pipes close
  }
}

if (!existsSync(exe)) { console.error(`build first: cargo build --release (no ${exe})`); process.exit(1); }
for (let r = 0; r < rounds; r++) for (const mode of MODES) {
  process.stdout.write(`round ${r + 1}/${rounds}, ${mode}… `);
  await run(mode);
  console.log('done');
}

const pct = (xs: number[], p: number) => {
  const s = [...xs].sort((a, b) => a - b);
  return s.length ? s[Math.min(s.length - 1, Math.floor((p / 100) * s.length))] : NaN;
};
const cell = (v: number) => (Number.isNaN(v) ? '–' : v.toFixed(2)).padStart(9);
console.log(`\n${dims}${fake ? ' made-up picture' : ' desktop'}, ${rounds} rounds, ${got.split.trip.length} full frames each ` +
  `(median ${pct(got.split.kb, 50).toFixed(0)} KB). Milliseconds.\n`);
console.log(`${''.padEnd(34)}${'one process'.padStart(20)}${'split'.padStart(20)}${'difference'.padStart(20)}`);
console.log(`${''.padEnd(34)}${'median'.padStart(10)}${'p99'.padStart(10)}${'median'.padStart(10)}${'p99'.padStart(10)}${'median'.padStart(10)}${'p99'.padStart(10)}`);
const rows: [string, keyof Samples][] = [
  ['first frame, warm', 'warm'], ['first frame, from idle', 'cold'], ['full frame, round trip', 'trip'],
  ['  of that, host capture to send', 'host'], ['  of that, travelling', 'travel'], ['engine to socket (host timing)', 'hop'],
];
for (const [label, key] of rows) {
  const a = got['one process'][key] as number[], b = got.split[key] as number[];
  if (!a.length && !b.length) continue;
  const [a50, a99, b50, b99] = [pct(a, 50), pct(a, 99), pct(b, 50), pct(b, 99)];
  console.log(`${label.padEnd(34)}${cell(a50)} ${cell(a99)} ${cell(b50)} ${cell(b99)} ${cell(b50 - a50)} ${cell(b99 - a99)}`);
}
console.log(`\nvitals messages during the streams: one process ${got['one process'].vitals}, split ${got.split.vitals}`);
if (lastVitals) console.log(`the last: ${lastVitals}`);
