// Counters for the stats panel. The per-second ones are reset by the tick in
// connection.ts, which also publishes a snapshot for the UI.
import { createSignal } from 'solid-js';

export const S = {
  bytesIn: 0, bytesOut: 0, frames: 0, rects: 0, host: 0, client: 0, rtt: 0, pongs: 0,
  totalIn: 0, totalOut: 0, lastSize: 0, lastKind: '',
};
/** Latest averages, kept across quiet seconds. */
export const L: { host: number | null; client: number | null; rtt: number | null } =
  { host: null, client: null, rtt: null };
/** KB/s received, one entry per second, newest last. */
export const rate: number[] = [];
export const RATE_SECONDS = 60;
export const ping = { n: 0, at: 0 };
export const opened = performance.now();
/** How long the screen took to show: from opening the socket (and, for the
 *  page's first connection, from navigating to the page) to the first frame
 *  drawn. Null until a connection made while watching has drawn one. */
export const first: { connect: number | null; page: number | null } = { connect: null, page: null };

export const countOut = (n: number) => { S.bytesOut += n; S.totalOut += n; };
export const countIn = (n: number) => { S.bytesIn += n; S.totalIn += n; };

export interface Shown { fps: number; kbIn: number; kbOut: number; rects: number }
/** Last second's figures. Set every second, also when unchanged, so totals
 *  and durations read alongside it refresh too. */
export const [shown, setShown] = createSignal<Shown>({ fps: 0, kbIn: 0, kbOut: 0, rects: 0 }, { equals: false });

export function tick() {
  const next = { fps: S.frames, kbIn: S.bytesIn / 1024, kbOut: S.bytesOut / 1024, rects: S.frames ? S.rects / S.frames : 0 };
  if (S.frames) { L.host = S.host / S.frames; L.client = S.client / S.frames; }
  if (S.pongs) L.rtt = S.rtt / S.pongs;
  rate.push(next.kbIn);
  if (rate.length > RATE_SECONDS) rate.shift();
  Object.assign(S, { bytesIn: 0, bytesOut: 0, frames: 0, rects: 0, host: 0, client: 0, rtt: 0, pongs: 0 });
  setShown(next);
}
