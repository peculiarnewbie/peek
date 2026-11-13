// A provider with a page of its own (a spike: see docs/frames.md).
//
//   peek provider add notes -- node /path/to/frame/provider.mjs
//
// It names its page's folder in what it says of itself. Peek serves that
// folder at /p/notes/ on this machine, and the hosted client shows it in a
// frame. The page talks to this program on a socket of its own: JSON as
// text, and samples as binary (32 floats, 30 times a second).

import { fileURLToPath } from 'node:url';

const OPEN = 1, CLOSE = 2, TEXT = 3, BINARY = 4, ABOUT = 7;

function send(kind, channel, payload) {
  const body = Buffer.isBuffer(payload) ? payload : Buffer.from(JSON.stringify(payload));
  const head = Buffer.alloc(13);
  head.writeUInt32LE(body.length, 0);
  head.writeUInt8(kind, 4);
  head.writeBigUInt64LE(channel, 5);
  process.stdout.write(Buffer.concat([head, body]));
}

const page = fileURLToPath(new URL('./page/', import.meta.url));
send(ABOUT, 0n, { page, places: [] });

/** Notes typed on any page, newest last. */
const notes = [];
/** Channels of open pages. */
const pages = new Set();

const everyone = msg => { for (const c of pages) send(TEXT, c, msg); };

let t = 0;
setInterval(() => {
  if (!pages.size) return;
  const samples = new Float32Array(32);
  for (let i = 0; i < samples.length; i++) {
    const x = t + i / samples.length;
    samples[i] = 0.5 + 0.3 * Math.sin(x * 2.1) + 0.15 * Math.sin(x * 7.3) + 0.05 * Math.random();
  }
  t += 1 / 30;
  for (const c of pages) send(BINARY, c, Buffer.from(samples.buffer));
}, 1000 / 30);

let pending = Buffer.alloc(0);
process.stdin.on('data', chunk => {
  pending = Buffer.concat([pending, chunk]);
  while (pending.length >= 13) {
    const length = pending.readUInt32LE(0);
    if (pending.length < 13 + length) break;
    const kind = pending.readUInt8(4), channel = pending.readBigUInt64LE(5);
    const payload = pending.subarray(13, 13 + length);
    pending = pending.subarray(13 + length);
    if (kind === OPEN) {
      let how = {};
      try { how = JSON.parse(payload.toString()); } catch {}
      if (how.from === 'page') {
        pages.add(channel);
        send(TEXT, channel, { t: 'notes', notes });
      }
    } else if (kind === CLOSE) {
      pages.delete(channel);
    } else if (kind === BINARY) {
      send(TEXT, channel, { t: 'got', bytes: payload.length });
    } else if (kind === TEXT) {
      let ask;
      try { ask = JSON.parse(payload.toString()); } catch { continue; }
      if (ask.t === 'do' && typeof ask.do?.note === 'string') {
        notes.push({ text: ask.do.note.slice(0, 500), at: Date.now() });
        everyone({ t: 'notes', notes });
      } else {
        send(TEXT, channel, { t: 'error', why: 'notes does not know that' });
      }
    }
  }
});
process.stdin.on('end', () => process.exit(0));
