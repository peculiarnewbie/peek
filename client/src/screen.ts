import { isTouch } from './state';

// The canvas showing the remote screen. One element for the whole page, made
// here so the socket, input handling and components can all reach it.
//
// The canvas is laid out to fit the window ("base" size, centred), then
// zoomed and panned with a CSS transform. Pointer positions are read with
// getBoundingClientRect, which includes the transform, so they stay right.

export const cv = document.createElement('canvas');
cv.id = 'screen';
cv.tabIndex = 0;
cv.width = 16;
cv.height = 9;

export const ctx = cv.getContext('2d', { alpha: false })!;

/** The area the canvas is centred in: the window above the keyboard dock.
 *  Fingers work anywhere in it, not only on the canvas. */
export const stage = document.createElement('div');
stage.id = 'stage';
stage.append(cv);

/** Keyboard input goes to the canvas; return focus there after UI clicks.
 *  Not on a touch screen: there, focus is the user's business (moving it
 *  would close the phone's keyboard). */
export const focusScreen = () => { if (!isTouch()) cv.focus({ preventScroll: true }); };

/** Zoom factor over the fitted size, and translation in CSS pixels. */
export const view = { z: 1, x: 0, y: 0 };
let baseW = 1, baseH = 1;
const viewListeners: (() => void)[] = [];
/** Called whenever the canvas moves on the page. */
export const onViewChange = (f: () => void) => { viewListeners.push(f); };

/** Height kept free at the bottom of the window, for the keyboard dock. */
let bottomInset = 0;
/** The area the remote screen is shown in: the window above the dock. */
export const areaW = () => innerWidth;
export const areaH = () => Math.max(innerHeight - bottomInset, 1);
export function setBottomInset(px: number) {
  if (px === bottomInset) return;
  bottomInset = px;
  document.documentElement.style.setProperty('--bottom-inset', px + 'px');
  layout();
}

/** At most this many CSS pixels per remote pixel when zoomed in. */
const MAX_PIXEL_ZOOM = 3;
const maxZoom = () => Math.max(2, (MAX_PIXEL_ZOOM * cv.width) / baseW);

/** Fit the canvas into the window, keeping its aspect ratio. */
export function layout() {
  const k = Math.min(areaW() / cv.width, areaH() / cv.height);
  baseW = Math.floor(cv.width * k);
  baseH = Math.floor(cv.height * k);
  cv.style.width = baseW + 'px';
  cv.style.height = baseH + 'px';
  view.z = Math.min(view.z, maxZoom());
  applyView();
}

export function resizeScreen(w: number, h: number) {
  if (cv.width === w && cv.height === h) return;
  cv.width = w;
  cv.height = h;
  layout();
}

/** Where the canvas's top-left corner sits when not zoomed. */
const baseOrigin = () => ({ x: (areaW() - baseW) / 2, y: (areaH() - baseH) / 2 });

/** The canvas as currently shown, in CSS pixels. */
export function shownRect() {
  const o = baseOrigin();
  return { left: o.x + view.x, top: o.y + view.y, width: baseW * view.z, height: baseH * view.z };
}

// Keep the zoomed canvas covering the area: no empty margin on a side
// where it is larger than the area, centred where it is smaller.
function clampView() {
  const o = baseOrigin(), aw = areaW(), ah = areaH();
  const w = baseW * view.z, h = baseH * view.z;
  view.x = w <= aw ? (aw - w) / 2 - o.x : Math.min(Math.max(view.x, aw - w - o.x), -o.x);
  view.y = h <= ah ? (ah - h) / 2 - o.y : Math.min(Math.max(view.y, ah - h - o.y), -o.y);
}

function applyView() {
  clampView();
  cv.style.transform = view.z === 1 && !view.x && !view.y
    ? '' : `translate(${view.x}px, ${view.y}px) scale(${view.z})`;
  for (const f of viewListeners) f();
}

/** A point on the page, as a position on the unzoomed canvas. */
export function toCanvas(px: number, py: number) {
  const r = shownRect();
  return { x: (px - r.left) / view.z, y: (py - r.top) / view.z };
}

/** Zoom to `z`, putting the unzoomed canvas point `at` under page point (px, py). */
export function zoomTo(z: number, at: { x: number; y: number }, px: number, py: number) {
  view.z = Math.min(Math.max(z, 1), maxZoom());
  const o = baseOrigin();
  view.x = px - o.x - at.x * view.z;
  view.y = py - o.y - at.y * view.z;
  applyView();
}

export function panBy(dx: number, dy: number) {
  view.x += dx;
  view.y += dy;
  applyView();
}

export const zoomed = () => view.z > 1;
