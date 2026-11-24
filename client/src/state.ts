// State shared by the UI and the connection. Plain signals, so imperative
// code (input, the socket) and components read and write the same values.
import { createSignal } from 'solid-js';
import { createStore } from 'solid-js';
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
  /** Images through the clipboard: Ctrl+V with one here puts it on the host's
   *  clipboard, and one copied there comes here (HTTPS only). This browser only. */
  clipImages: boolean;
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
  clipImages: store.get('clipimg') !== '0',
});

/** The largest image sent either way, as PNG (MAX_CLIPBOARD_IMAGE in server.rs). */
export const MAX_CLIP_IMAGE = 8 << 20;
/** This page may put an image on the clipboard: only an HTTPS page may. */
export const COPY_IMAGES = window.isSecureContext && typeof ClipboardItem !== 'undefined' && !!navigator.clipboard?.write;
/** Whether the host should send images copied there. */
export const wantImages = () => cfg.clipImages && COPY_IMAGES;

export interface Monitor { w: number; h: number }

// From the host.
export const [monitors, setMonitors] = createSignal<Monitor[]>([]);
export const [viewOnly, setViewOnly] = createSignal(false);
export const [clipboardOn, setClipboardOn] = createSignal(false);
/** The window in front on the host is elevated and ignores our input. */
export const [blocked, setBlocked] = createSignal(false);

// Connection.
export const [live, setLive] = createSignal(false);
/** A button on the status card: a link, or something to do. */
export interface MessageAction { label: string; href?: string; run?: () => void }
const [messageText, setMessageText] = createSignal('');
const [messageAction, setMessageAction] = createSignal<MessageAction | null>(null);
/** Centred status card; empty hides it. */
export const message = messageText;
export { messageAction };
/** Shows `text` on the status card, with a button if given; a new message
 *  without one drops the old button. */
export function setMessage(text: string, action: MessageAction | null = null) {
  setMessageText(text);
  setMessageAction(action);
}

// Sleep: the connection stays open but the host sends nothing.
export const [sleeping, setSleeping] = createSignal(false);
/** Paused by "Pause when unfocused" rather than by the user. */
export const [autoSlept, setAutoSlept] = createSignal(false);
/** A provider's own page is shown in place of the screen (Frame.tsx). */
export const [onPage, setOnPage] = createSignal(false);
/** The host only sends the screen while this is true. */
export const active = () => !document.hidden && !sleeping() && !onPage();

// Panels.
export const [settingsOpen, setSettingsOpen] = createSignal(false);
export const [pinned, setPinned] = createSignal(store.get('pin') === '1');
export type StatsMode = 'off' | 'open';
// An older 'folded' (from before hover) counts as open.
export const [statsMode, setStatsMode] = createSignal<StatsMode>(
  ['open', 'folded'].includes(store.get('stats') ?? '') ? 'open' : 'off');
export const [statsPinned, setStatsPinned] = createSignal(store.get('stats-pin') === '1');
export const [fullscreen, setFullscreen] = createSignal(false);
/** On-screen keyboard dock shown on a non-touch browser, as a fallback. */
export const [kbdOpen, setKbdOpen] = createSignal(false);

// Touch screens.
export type TouchMode = 'trackpad' | 'direct';
/** Trackpad: a finger moves a cursor, as on a laptop. Direct: the finger is the cursor. */
export const [touchMode, setTouchMode] = createSignal<TouchMode>(
  store.get('touch') === 'direct' ? 'direct' : 'trackpad');
/** This browser has been used with a finger (or has no fine pointer at all). */
export const [isTouch, setIsTouch] = createSignal(matchMedia('(pointer: coarse)').matches);
/** The toolbar, opened by a tap; there is no hover to open it on a touch screen. */
export const [barOpen, setBarOpen] = createSignal(false);
