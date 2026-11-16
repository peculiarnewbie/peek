// Things the UI and input handling both do.
import { flush } from 'solid-js';
import { send } from './connection';
import {
  active, cfg, pinned, setAutoSlept, setCfg, setPinned, setSettingsOpen, setSleeping, setStatsMode,
  setStatsPinned, setTouchMode, sleeping, statsPinned, touchMode, type Cfg, type StatsMode,
} from './state';
import { focusScreen } from './screen';
import { store } from './util';

/** Losing focus means we may never see the key-up; have the host let go of everything. */
export const release = { all: () => {} };

// The connection stays open, so resuming is instant, but the host stops
// capturing and sending (and frees the capture after a few seconds).
export function setSleep(v: boolean, auto = false) {
  if (sleeping() === v) return;
  setSleeping(v); setAutoSlept(v && auto);
  flush();   // so active() below sees it
  if (v) release.all();
  send({ t: 'vis', v: active() });
  if (!v) focusScreen();
}

export function applyCfg(change: Partial<Cfg>) {
  // (A store's new values are read only after the update lands, so from `next`.)
  const next: Cfg = { ...cfg, ...change };
  setCfg(s => { Object.assign(s, change); });
  store.set('q', next.quality); store.set('scale', next.scale); store.set('soft', next.soft ? 1 : 0);
  store.set('lossless', next.lossless ? 1 : 0);
  store.set('fps', next.fps); store.set('mon', next.mon); store.set('pause', next.autoSleep ? 1 : 0);
  store.set('pasteimg', next.pasteImages ? 1 : 0);
  send({ t: 'cfg', q: next.quality, scale: next.scale, mon: next.mon, fps: next.fps, soft: next.soft, lossless: next.lossless });
}

export function showSettings(open: boolean) {
  setSettingsOpen(open);
  focusScreen();
}

export function setStats(mode: StatsMode) {
  setStatsMode(mode);
  store.set('stats', mode);
}

export function togglePin() {
  const v = !pinned();
  setPinned(v);
  store.set('pin', v ? 1 : 0);
  focusScreen();
}

export function toggleTouchMode() {
  const v = touchMode() === 'trackpad' ? 'direct' : 'trackpad';
  setTouchMode(v);
  store.set('touch', v);
}

export function toggleStatsPin() {
  const v = !statsPinned();
  setStatsPinned(v);
  store.set('stats-pin', v ? 1 : 0);
  focusScreen();
}

export async function toggleFullscreen() {
  if (document.fullscreenElement) await document.exitFullscreen();
  else {
    await document.documentElement.requestFullscreen();
    const kb = (navigator as Navigator & { keyboard?: { lock?: () => Promise<void> } }).keyboard;
    kb?.lock?.().catch(() => {});
  }
  focusScreen();
}
