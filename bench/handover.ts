// SPIKE: does a connection outlive its screen provider? One socket, kept
// busy with full frames about ten times a second, watched for a while:
//
//   PEEK_TOKEN=… bun bench/handover.ts ws://127.0.0.1:9182/ws [seconds]
//
// Meanwhile, end the screen provider, sign out, or switch user. It reports
// each time the picture stopped and for how long, and whether the socket
// ever closed.
/** Must match PROTOCOL in src/server.rs. */
const PROTOCOL = 4;

const [url = 'ws://127.0.0.1:9182/ws', secondsArg = '30'] = process.argv.slice(2);
const seconds = Number(secondsArg);
const token = process.env.PEEK_TOKEN ?? '';
const origin = url.replace(/^ws/, 'http').replace(/\/ws.*$/, '');
const GAP_MS = 400;

const q = new URLSearchParams({ v: String(PROTOCOL), token, q: 'med', scale: '1', fps: '60', soft: '0', lossless: '0', mon: '0', vis: '1' });
const t0 = performance.now();
const at = () => `${((performance.now() - t0) / 1000).toFixed(2).padStart(6)} s`;
const ws = new WebSocket(`${url}?${q}`, { headers: { Origin: origin } } as any);
ws.binaryType = 'arraybuffer';

let frames = 0, hellos = 0, vitals = 0, lastFrame = 0, lastSeq = 0, quality = 'med', longest = 0, closed = '';
const ask = () => {
  if (ws.readyState !== WebSocket.OPEN) return;
  quality = quality === 'med' ? 'high' : 'med';
  ws.send(JSON.stringify({ t: 'cfg', q: quality }));
  ws.send(JSON.stringify({ t: 'ack', s: lastSeq }));
};
ws.onopen = () => console.log(`${at()}  socket open`);
ws.onmessage = e => {
  if (typeof e.data === 'string') {
    const m = JSON.parse(e.data);
    if (m.t === 'vitals') vitals++;
    // Every change of setting brings a hello; only the first is news, and
    // the one after a gap, which is a new provider introducing the screen.
    if (m.t === 'hello') {
      hellos++;
      if (!lastFrame || performance.now() - lastFrame > GAP_MS) {
        console.log(`${at()}  hello: ${m.monitors.map((x: any) => `${x.w}×${x.h}`).join(', ') || 'no monitors'}`);
      }
    }
    return;
  }
  const now = performance.now();
  if (lastFrame) {
    const gap = now - lastFrame;
    longest = Math.max(longest, gap);
    if (gap > GAP_MS) console.log(`${at()}  the picture is back, after ${gap.toFixed(0)} ms without a frame`);
  } else console.log(`${at()}  first frame`);
  lastFrame = now;
  lastSeq = new DataView(e.data as ArrayBuffer).getUint32(1, true);
  frames++;
  setTimeout(ask, 100);
};
ws.onclose = e => { closed = `${e.code} ${e.reason}`.trim(); console.log(`${at()}  SOCKET CLOSED (${closed})`); };

setTimeout(() => {
  console.log(`\n${seconds} s: ${frames} frames, ${hellos} hello(s), ${vitals} vitals messages, longest wait for a frame ${longest.toFixed(0)} ms, ` +
    (closed ? `socket closed (${closed})` : 'socket open throughout'));
  ws.close();
  process.exit(closed ? 1 : 0);
}, seconds * 1000);
