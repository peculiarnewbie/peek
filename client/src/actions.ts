// Things the UI and input handling both do.
import { batch } from 'solid-js';
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
  batch(() => { setSleeping(v); setAutoSlept(v && auto); });
  if (v) release.all();
  send({ t: 'vis', v: active() });
  if (!v) focusScreen();
}

export function applyCfg(change: Partial<Cfg>) {
  setCfg(change);
  store.set('q', cfg.quality); store.set('scale', cfg.scale); store.set('soft', cfg.soft ? 1 : 0);
  store.set('lossless', cfg.lossless ? 1 : 0);
  store.set('fps', cfg.fps); store.set('mon', cfg.mon); store.set('pause', cfg.autoSleep ? 1 : 0);
  send({ t: 'cfg', q: cfg.quality, scale: cfg.scale, mon: cfg.mon, fps: cfg.fps, soft: cfg.soft, lossless: cfg.lossless });
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
  setPinned(!pinned());
  store.set('pin', pinned() ? 1 : 0);
  focusScreen();
}

export function toggleTouchMode() {
  setTouchMode(touchMode() === 'trackpad' ? 'direct' : 'trackpad');
  store.set('touch', touchMode());
}

export function toggleStatsPin() {
  setStatsPinned(!statsPinned());
  store.set('stats-pin', statsPinned() ? 1 : 0);
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
