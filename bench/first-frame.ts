// How long peek takes from "open the socket" to "the whole first frame has
// arrived": the wait that peek exists to keep short.
//
//   bun bench/first-frame.ts ws://127.0.0.1:9182/ws [runs] [token] [--warm]
//
// Each run opens a fresh connection, waits for the first frame, and closes.
// Runs are cold by default: the script waits out the host's idle release
// (5 s), so every connection finds the capture shut down, as it is when you
// connect to an idle machine. `--warm` connects again straight away instead.
//
// The browser then decodes and draws the frame, a few more milliseconds that
// the client's stats panel shows as "First frame".
/** Must match PROTOCOL in src/server.rs. */
const PROTOCOL = 4;

const args = process.argv.slice(2);
const warm = args.includes('--warm');
const [url = 'ws://127.0.0.1:9182/ws', runsArg = '20', token = ''] = args.filter(a => !a.startsWith('--'));
const runs = Number(runsArg);
const origin = url.replace(/^ws/, 'http').replace(/\/ws.*$/, '');
const IDLE_RELEASE_MS = 5000;

/** Milliseconds from opening the socket; `encode` is the host's own count from
 *  capture to send (the frame header's host time). */
interface Run { open: number; hello: number; frame: number; encode: number; bytes: number; w: number; h: number }

function once(): Promise<Run> {
  return new Promise((resolve, reject) => {
    const q = new URLSearchParams({ v: String(PROTOCOL), token, q: 'med', scale: '1', fps: '60', soft: '0', mon: '0', vis: '1' });
    const t0 = performance.now();
    const r: Partial<Run> = {};
    const ws = new WebSocket(`${url}?${q}`, { headers: { Origin: origin } } as any);
    ws.binaryType = 'arraybuffer';
    const timer = setTimeout(() => { ws.close(); reject(new Error('no frame within 10 s')); }, 10000);
    ws.onopen = () => { r.open = performance.now() - t0; };
    ws.onmessage = e => {
      if (typeof e.data === 'string') {
        if (JSON.parse(e.data).t === 'hello') r.hello = performance.now() - t0;
        return;
      }
      r.frame = performance.now() - t0;
      const dv = new DataView(e.data as ArrayBuffer);
      r.bytes = (e.data as ArrayBuffer).byteLength;
      r.w = dv.getUint16(5, true);
      r.h = dv.getUint16(7, true);
      r.encode = dv.getUint32(11, true) / 1000;
      clearTimeout(timer);
      ws.close();
      resolve(r as Run);
    };
    ws.onclose = e => { clearTimeout(timer); if (r.frame == null) reject(new Error(`closed (${e.code} ${e.reason})`)); };
  });
}

const pct = (xs: number[], p: number) => {
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor((p / 100) * s.length))];
};
const fmt = (n: number) => n.toFixed(1).padStart(7);

const results: Run[] = [];
console.log(`${runs} ${warm ? 'warm' : 'cold'} connections to ${url}`);
for (let i = 0; i < runs; i++) {
  if (!warm || i === 0) await Bun.sleep(IDLE_RELEASE_MS + 1000);
  const r = await once();
  results.push(r);
  console.log(`  #${String(i + 1).padStart(2)}  open ${fmt(r.open)} ms  hello ${fmt(r.hello)} ms  ` +
    `first frame ${fmt(r.frame)} ms  (capture to send ${fmt(r.encode)} ms, ${r.w}×${r.h}, ${(r.bytes / 1024).toFixed(0)} KB)`);
  if (warm) await Bun.sleep(200);
}
const LABELS = { open: 'socket open', hello: 'hello', frame: 'first frame', encode: 'capture to send' } as const;
for (const [key, label] of Object.entries(LABELS) as [keyof typeof LABELS, string][]) {
  const xs = results.map(r => r[key]);
  console.log(`${label.padEnd(15)} median ${fmt(pct(xs, 50))} ms   p90 ${fmt(pct(xs, 90))} ms   ` +
    `min ${fmt(Math.min(...xs))} ms   max ${fmt(Math.max(...xs))} ms`);
}
