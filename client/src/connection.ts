import { SCREEN_PLUGIN } from './plugin-mode';
// The WebSocket to the host: connecting, reconnecting, decoding frames.
import {
  active, cfg, setBlocked, setCfg, setClipboardOn, setLive, setMessage, setMonitors, setViewOnly,
  statsMode, viewOnly, type Monitor,
} from './state';
import { countIn, countOut, first, ping, S, tick } from './stats';
import { cv, ctx, resizeScreen } from './screen';
import { BLOCKED_HELP, current, HOSTED, localAccess } from './hosted';
import { store } from './util';
import { dropPages, fromProvider } from './views';
import type { PairOffer } from './components/PairDevice';

/** Must match the host's wire format version. */
const PROTOCOL = 4;
// Close codes from the host (server.rs).
const CLOSE_BAD_ORIGIN = 4401;
const CLOSE_BAD_TOKEN = 4403;
const CLOSE_OUTDATED = 4426;

// Access token: arrives once in the URL fragment, then lives in localStorage.
// A host started with --no-token needs none, so connecting without one is fine.
// (The hosted client pairs instead: see hosted.ts.)
function takeToken() {
  const t = location.hash.slice(1);
  if (t) { store.set('token', t); history.replaceState(null, '', location.pathname); }
  return t;
}
const token = HOSTED || SCREEN_PLUGIN ? '' : takeToken() || store.get('token') || '';
// Entering the full URL in a tab that already shows this page only changes the
// fragment, which does not reload the page, so pick the token up here.
if (!HOSTED && !SCREEN_PLUGIN) addEventListener('hashchange', () => { if (takeToken()) location.reload(); });

/** Where the socket goes: the host that served this page, or the chosen machine. */
function target(): { url: string; token: string; name: string } | null {
  if (SCREEN_PLUGIN) {
    const name = location.pathname.split('/')[2];
    const ticket = new URLSearchParams(location.hash.slice(1)).get('ticket') || '';
    return { url: `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/api/frame/${encodeURIComponent(name)}/ws`, token: ticket, name };
  }
  if (!HOSTED) {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    return { url: `${proto}://${location.host}/ws`, token, name: 'the host' };
  }
  const m = current();
  return m && { url: `wss://${m.host}/ws`, token: m.token, name: m.name };
}
/** The machine's address and token, for what is not the screen's socket. */
export function machine(): { base: string; token: string; name: string } | null {
  const t = target();
  return t && { base: t.url.replace(/^ws/, 'http').replace(/\/ws$/, ''), token: t.token, name: t.name };
}
let retryTimer: ReturnType<typeof setTimeout> | undefined;
/** Failed attempts in a row, to say so once a machine stays away. */
let failures = 0;

let ws: WebSocket | null = null;
let retry = 250;
/** When the current socket was opened, while its first frame is still to come
 *  (and was asked for: one opened in a hidden tab waits for it to show). */
let firstFrameFrom: number | null = null;
/** Whether that socket was opened as the page loaded, so the page load counts too. */
let fromPageLoad = false;
/** Still running the page's startup code (a later connect is a click or a retry). */
let startup = true;
setTimeout(() => { startup = false; });
/** Frames are decoded asynchronously but drawn in order. */
let chain = Promise.resolve();

/** Set by the input code: the host's clipboard changed / its pointer shape changed. */
export const hooks = {
  clip: (_text: string) => {},
  cursor: (_css: string) => {},
  paircode: (_offer: PairOffer) => {},
};

export function send(o: object) {
  if (!ws || ws.readyState !== WebSocket.OPEN) return;
  const text = JSON.stringify(o);
  countOut(text.length);
  ws.send(text);
}

/** Drop the connection (to switch machines), without reconnecting. */
export function disconnect() {
  clearTimeout(retryTimer);
  const sock = ws;
  ws = null;
  sock?.close();
  setLive(false);
  setBlocked(false);
  dropPages();
  setMonitors([]);
  chain = Promise.resolve();
  ctx.clearRect(0, 0, cv.width, cv.height);
}

export function connect() {
  clearTimeout(retryTimer);
  const t = target();
  if (!t) { setMessage(''); return; }
  const q = new URLSearchParams({
    v: String(PROTOCOL), token: t.token, q: cfg.quality, scale: String(cfg.scale), fps: String(cfg.fps),
    soft: cfg.soft ? '1' : '0', lossless: cfg.lossless ? '1' : '0', mon: String(cfg.mon), vis: active() ? '1' : '0',
  });
  if (SCREEN_PLUGIN) { q.delete('token'); q.set('ticket', t.token); }
  firstFrameFrom = active() ? performance.now() : null;
  fromPageLoad = startup;
  const sock = ws = new WebSocket(`${t.url}?${q}`);
  sock.binaryType = 'arraybuffer';
  sock.onopen = () => {
    retry = 250;
    failures = 0;
    setMessage('');
    setLive(true);
    // Pausing or resuming while the socket was still connecting (focus often
    // arrives just after a reload) could not be sent; restate where we are now.
    if (SCREEN_PLUGIN) send({ t: 'cfg', q: cfg.quality, scale: cfg.scale, mon: cfg.mon, fps: cfg.fps, soft: cfg.soft, lossless: cfg.lossless });
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
    dropPages();
    if (e.code === CLOSE_BAD_TOKEN) {
      if (SCREEN_PLUGIN) {
        setMessage('Authorization expired. Reopening…');
        if (parent !== window) parent.postMessage({ t: 'renew-ticket' }, '*');
        return;
      }
      if (HOSTED) {
        setMessage(`This browser is no longer paired with ${t.name}. Run \`peek pair\` there to pair it again.`);
        return;
      }
      store.set('token', '');
      setMessage((token ? 'Wrong access token.' : 'This host needs an access token.') +
        ' Open the full URL the host printed (it ends in #token).');
      return;
    }
    if (e.code === CLOSE_BAD_ORIGIN) {
      setMessage(HOSTED
        ? `${t.name} does not accept this site. Start peek there with --web ${location.origin}.`
        : 'The host refused this address. Open it by its IP address or Tailscale name.');
      return;
    }
    if (e.code === CLOSE_OUTDATED) {
      // The host was upgraded while this tab was open. Reload once to pick up
      // its new client; if that does not help, say so instead of looping.
      if (Date.now() - Number(sessionStorage.getItem('peek-reloaded') || 0) > 10000) {
        sessionStorage.setItem('peek-reloaded', String(Date.now()));
        location.reload();
      } else setMessage(HOSTED
        ? `${t.name} runs a different version of peek than this site. Update peek there.`
        : 'This page does not match the host version. Reload it.');
      return;
    }
    failures++;
    setMessage(!HOSTED || failures <= 2 ? 'Reconnecting…'
      : localAccess() === 'denied' ? BLOCKED_HELP
      : `Cannot reach ${t.name}. Retrying… (Is Tailscale connected on this device?)`);
    retryTimer = setTimeout(connect, retry);
    retry = Math.min(retry * 2, 3000);
  };
}

type HostMsg =
  | { t: 'hello'; monitors: Monitor[]; mon: number; viewOnly: boolean; clipboard: boolean }
  | { t: 'cur'; c: string }
  | { t: 'blocked'; v: boolean }
  | { t: 'clip'; s: string }
  | { t: 'pong'; n: number }
  // From one of the host's other providers, on its channel of this socket.
  | { t: 'from'; p: string; m: Parameters<typeof fromProvider>[1] }
  | ({ t: 'paircode' } & PairOffer);

function onJson(m: HostMsg) {
  switch (m.t) {
    case 'hello':
      setMonitors(m.monitors); setCfg('mon', m.mon); setViewOnly(m.viewOnly); setClipboardOn(m.clipboard);
      break;
    case 'cur':
      cv.style.cursor = m.c;
      hooks.cursor(m.c);
      break;
    case 'blocked':
      setBlocked(m.v && !viewOnly());
      break;
    case 'clip':
      hooks.clip(m.s);
      break;
    case 'paircode':
      hooks.paircode(m);
      break;
    case 'from':
      fromProvider(m.p, m.m);
      break;
    case 'pong':
      if (m.n === ping.n) { S.rtt += performance.now() - ping.at; S.pongs++; }
      break;
  }
}

// Frame: u8 kind, u32 seq, u16 width, u16 height, u16 count, u32 host time
// (µs from capture to send), u8 JPEG quality, u8 flags (1 = full colour
// resolution, 2 = sharp, 4 = encoded at half size, 8 = format bytes), then
// per rectangle u16 x, u16 y, u16 w, u16 h (where on the canvas), with flag 8
// a u8 format (0 JPEG, 1 lossless WebP), u32 length, image bytes. A JPEG
// smaller than its rectangle is stretched to fill it. Rectangles are drawn in
// order: JPEGs over the flat-painted tiles of the lossless pieces before them.
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
    const format = flags & 8 ? dv.getUint8(off + 8) : 0;
    off += flags & 8 ? 9 : 8;
    const len = dv.getUint32(off, true);
    off += 4;
    const blob = new Blob([new Uint8Array(buf, off, len)], { type: format === 1 ? 'image/webp' : 'image/jpeg' });
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
  if (firstFrameFrom != null && sock === ws) {
    const now = performance.now();
    first.connect = now - firstFrameFrom;
    first.page = fromPageLoad ? now : null;   // performance.now() counts from navigation
    firstFrameFrom = null;
  }
  S.frames++; S.rects += n;
  S.host += hostUs / 1000; S.client += performance.now() - arrived;
  S.lastSize = buf.byteLength;
  S.lastKind = `q${jpegQ} ${flags & 1 ? '4:4:4' : '4:2:0'}, ${flags & 2 ? 'sharp' : 'moving'}` +
    (flags & 4 ? ', half size' : '') + (flags & 8 ? ', lossless text' : '');
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
