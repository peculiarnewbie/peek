// The WebSocket to the host: connecting, reconnecting, decoding frames.
import {
  active, cfg, setBlocked, setCfg, setClipboardOn, setLive, setMessage, setMonitors, setViewOnly,
  statsMode, viewOnly, type Monitor,
} from './state';
import { countIn, countOut, ping, S, tick } from './stats';
import { cv, ctx, resizeScreen } from './screen';
import { store } from './util';

/** Must match the host's wire format version. */
const PROTOCOL = 4;
// Close codes from the host (server.rs).
const CLOSE_BAD_ORIGIN = 4401;
const CLOSE_BAD_TOKEN = 4403;
const CLOSE_OUTDATED = 4426;

// Access token: arrives once in the URL fragment, then lives in localStorage.
// A host started with --no-token needs none, so connecting without one is fine.
function takeToken() {
  const t = location.hash.slice(1);
  if (t) { store.set('token', t); history.replaceState(null, '', location.pathname); }
  return t;
}
const token = takeToken() || store.get('token') || '';
// Entering the full URL in a tab that already shows this page only changes the
// fragment, which does not reload the page, so pick the token up here.
addEventListener('hashchange', () => { if (takeToken()) location.reload(); });

let ws: WebSocket | null = null;
let retry = 250;
/** Frames are decoded asynchronously but drawn in order. */
let chain = Promise.resolve();

/** The host's clipboard changed. Set by the clipboard code. */
export const hooks = { clip: (_text: string) => {} };

export function send(o: object) {
  if (!ws || ws.readyState !== WebSocket.OPEN) return;
  const text = JSON.stringify(o);
  countOut(text.length);
  ws.send(text);
}

export function connect() {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  const q = new URLSearchParams({
    v: String(PROTOCOL), token, q: cfg.quality, scale: String(cfg.scale), fps: String(cfg.fps),
    soft: cfg.soft ? '1' : '0', mon: String(cfg.mon), vis: active() ? '1' : '0',
  });
  const sock = ws = new WebSocket(`${proto}://${location.host}/ws?${q}`);
  sock.binaryType = 'arraybuffer';
  sock.onopen = () => {
    retry = 250;
    setMessage('');
    setLive(true);
    // Pausing or resuming while the socket was still connecting (focus often
    // arrives just after a reload) could not be sent; restate where we are now.
    send({ t: 'vis', v: active() });
  };
  sock.onmessage = e => {
    if (typeof e.data === 'string') {
      countIn(e.data.length);
      onJson(JSON.parse(e.data));
    } else {
      const buf = e.data as ArrayBuffer;
      countIn(buf.byteLength);
      const arrived = performance.now();
      chain = chain.then(() => onFrame(sock, buf, arrived)).catch(err => console.error(err));
    }
  };
  sock.onclose = e => {
    if (ws !== sock) return;
    setLive(false);
    setBlocked(false);
    if (e.code === CLOSE_BAD_TOKEN) {
      store.set('token', '');
      setMessage((token ? 'Wrong access token.' : 'This host needs an access token.') +
        ' Open the full URL the host printed (it ends in #token).');
      return;
    }
    if (e.code === CLOSE_BAD_ORIGIN) {
      setMessage('The host refused this address. Open it by its IP address or Tailscale name.');
      return;
    }
    if (e.code === CLOSE_OUTDATED) {
      // The host was upgraded while this tab was open. Reload once to pick up
      // its new client; if that does not help, say so instead of looping.
      if (Date.now() - Number(sessionStorage.getItem('peek-reloaded') || 0) > 10000) {
        sessionStorage.setItem('peek-reloaded', String(Date.now()));
        location.reload();
      } else setMessage('This page does not match the host version. Reload it.');
      return;
    }
    setMessage('Reconnecting…');
    setTimeout(connect, retry);
    retry = Math.min(retry * 2, 3000);
  };
}

type HostMsg =
  | { t: 'hello'; monitors: Monitor[]; mon: number; viewOnly: boolean; clipboard: boolean }
  | { t: 'cur'; c: string }
  | { t: 'blocked'; v: boolean }
  | { t: 'clip'; s: string }
  | { t: 'pong'; n: number };

function onJson(m: HostMsg) {
  switch (m.t) {
    case 'hello':
      setMonitors(m.monitors); setCfg('mon', m.mon); setViewOnly(m.viewOnly); setClipboardOn(m.clipboard);
      break;
    case 'cur':
      cv.style.cursor = m.c;
      break;
    case 'blocked':
      setBlocked(m.v && !viewOnly());
      break;
    case 'clip':
      hooks.clip(m.s);
      break;
    case 'pong':
      if (m.n === ping.n) { S.rtt += performance.now() - ping.at; S.pongs++; }
      break;
  }
}

// Frame: u8 kind, u32 seq, u16 width, u16 height, u16 count, u32 host time
// (µs from capture to send), u8 JPEG quality, u8 flags (1 = full colour
// resolution, 2 = sharp, 4 = encoded at half size), then per rectangle
// u16 x, u16 y, u16 w, u16 h (where on the canvas), u32 length, JPEG bytes.
// A JPEG smaller than its rectangle is stretched to fill it.
async function onFrame(sock: WebSocket, buf: ArrayBuffer, arrived: number) {
  const dv = new DataView(buf);
  const seq = dv.getUint32(1, true), W = dv.getUint16(5, true), H = dv.getUint16(7, true);
  const n = dv.getUint16(9, true), hostUs = dv.getUint32(11, true);
  const jpegQ = dv.getUint8(15), flags = dv.getUint8(16);
  const jobs = [];
  let off = 17;
  for (let i = 0; i < n; i++) {
    const x = dv.getUint16(off, true), y = dv.getUint16(off + 2, true);
    const w = dv.getUint16(off + 4, true), h = dv.getUint16(off + 6, true);
    const len = dv.getUint32(off + 8, true);
    off += 12;
    const blob = new Blob([new Uint8Array(buf, off, len)], { type: 'image/jpeg' });
    jobs.push(createImageBitmap(blob).then(bm => ({ x, y, w, h, bm })));
    off += len;
  }
  const parts = await Promise.all(jobs);
  resizeScreen(W, H);
  for (const p of parts) { ctx.drawImage(p.bm, p.x, p.y, p.w, p.h); p.bm.close(); }
  if (sock.readyState === WebSocket.OPEN) {
    const ack = JSON.stringify({ t: 'ack', s: seq });
    sock.send(ack);
    countOut(ack.length);
  }
  S.frames++; S.rects += n;
  S.host += hostUs / 1000; S.client += performance.now() - arrived;
  S.lastSize = buf.byteLength;
  S.lastKind = `q${jpegQ} ${flags & 1 ? '4:4:4' : '4:2:0'}, ${flags & 2 ? 'sharp' : 'moving'}` +
    (flags & 4 ? ', half size' : '');
}

// Round-trip probe, only while someone is looking at the numbers.
setInterval(() => {
  if (statsMode() !== 'off' && active()) {
    ping.n = (ping.n + 1) >>> 0;
    ping.at = performance.now();
    send({ t: 'ping', n: ping.n });
  }
}, 500);
setInterval(tick, 1000);
