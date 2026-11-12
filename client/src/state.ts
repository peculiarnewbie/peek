// State shared by the UI and the connection. Plain signals, so imperative
// code (input, the socket) and components read and write the same values.
import { createSignal } from 'solid-js';
import { createStore } from 'solid-js/store';
import { store } from './util';

/** JPEG quality while changing / once still, per setting (mirrors engine.rs). */
export const QUALITY = { lowest: [30, 65], low: [45, 75], med: [70, 90], high: [85, 95] } as const;
export type Quality = keyof typeof QUALITY;
export const FPS_STEPS = [60, 30, 15, 10];

export interface Cfg {
  quality: Quality;
  scale: 1 | 2;
  /** Changing regions at half resolution. */
  soft: boolean;
  /** Text and flat UI as lossless WebP; photo-like regions stay JPEG. */
  lossless: boolean;
  fps: number;
  mon: number;
  /** Pause while the window is in the background. This browser only; never sent to the host. */
  autoSleep: boolean;
}

const savedQuality = store.get('q');
export const [cfg, setCfg] = createStore<Cfg>({
  quality: savedQuality && savedQuality in QUALITY ? savedQuality as Quality : 'med',
  scale: store.get('scale') === '2' ? 2 : 1,
  soft: store.get('soft') === '1',
  lossless: store.get('lossless') !== '0',
  fps: FPS_STEPS.includes(Number(store.get('fps'))) ? Number(store.get('fps')) : 60,
  mon: parseInt(store.get('mon') ?? '', 10) || 0,
  // Saved as 'pause' since it became the default: every settings change used to
  // save 'autosleep' too, as off, whether or not anyone had picked it.
  autoSleep: store.get('pause') !== '0',
});

export interface Monitor { w: number; h: number }

// From the host.
export const [monitors, setMonitors] = createSignal<Monitor[]>([]);
export const [viewOnly, setViewOnly] = createSignal(false);
export const [clipboardOn, setClipboardOn] = createSignal(false);
/** The window in front on the host is elevated and ignores our input. */
export const [blocked, setBlocked] = createSignal(false);

/** SPIKE: what the machine is doing, from the host's vitals provider. */
export interface Vitals {
  cpu: number | null;
  cores: number;
  mem: { used: number; total: number } | null;
  disks: { name: string; used: number; total: number }[];
  /** Seconds since the machine started. */
  up: number;
}
/** Null from a host that has no vitals provider (any peek without --split). */
export const [vitals, setVitals] = createSignal<Vitals | null>(null);

// Connection.
export const [live, setLive] = createSignal(false);
/** Centred status card; empty hides it. */
export const [message, setMessage] = createSignal('');

// Sleep: the connection stays open but the host sends nothing.
export const [sleeping, setSleeping] = createSignal(false);
/** Paused by "Pause when unfocused" rather than by the user. */
export const [autoSlept, setAutoSlept] = createSignal(false);
/** The host only sends the screen while this is true. */
export const active = () => !document.hidden && !sleeping();

// Panels.
export const [settingsOpen, setSettingsOpen] = createSignal(false);
export const [pinned, setPinned] = createSignal(store.get('pin') === '1');
export type StatsMode = 'off' | 'open';
// An older 'folded' (from before hover) counts as open.
export const [statsMode, setStatsMode] = createSignal<StatsMode>(
  ['open', 'folded'].includes(store.get('stats') ?? '') ? 'open' : 'off');
export const [statsPinned, setStatsPinned] = createSignal(store.get('stats-pin') === '1');
export const [fullscreen, setFullscreen] = createSignal(false);

// Touch screens.
export type TouchMode = 'trackpad' | 'direct';
/** Trackpad: a finger moves a cursor, as on a laptop. Direct: the finger is the cursor. */
export const [touchMode, setTouchMode] = createSignal<TouchMode>(
  store.get('touch') === 'direct' ? 'direct' : 'trackpad');
/** This browser has been used with a finger (or has no fine pointer at all). */
export const [isTouch, setIsTouch] = createSignal(matchMedia('(pointer: coarse)').matches);
/** The toolbar, opened by a tap; there is no hover to open it on a touch screen. */
export const [barOpen, setBarOpen] = createSignal(false);
