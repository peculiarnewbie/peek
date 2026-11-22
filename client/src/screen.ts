import { isTouch } from './state';

// The canvas showing the remote screen. One element for the whole page, made
// here so the socket, input handling and components can all reach it.
//
// The canvas is laid out to fit the window ("base" size, centred), then
// positioned and zoomed with a CSS transform. Input uses the same coordinates.

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
let baseW = cv.width, baseH = cv.height;
let baseX = 0, baseY = 0;
let left = 0, top = 0;
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
  const aw = areaW(), ah = areaH();
  const k = Math.min(aw / cv.width, ah / cv.height);
  baseW = Math.floor(cv.width * k);
  baseH = Math.floor(cv.height * k);
  baseX = (aw - baseW) / 2;
  baseY = (ah - baseH) / 2;
  cv.style.width = baseW + 'px';
  cv.style.height = baseH + 'px';
  view.z = Math.min(view.z, maxZoom());
  applyView();
}
document.addEventListener('fullscreenchange', layout);

export function resizeScreen(w: number, h: number) {
  if (cv.width === w && cv.height === h) return;
  cv.width = w;
  cv.height = h;
  layout();
}

/** The canvas as currently shown, in CSS pixels. */
export function shownRect() {
  return { left, top, width: baseW * view.z, height: baseH * view.z };
}

// Keep the zoomed canvas covering the area: no empty margin on a side
// where it is larger than the area, centred where it is smaller.
function applyView() {
  const aw = areaW(), ah = areaH();
  const w = baseW * view.z, h = baseH * view.z;
  view.x = w <= aw ? (aw - w) / 2 - baseX : Math.min(Math.max(view.x, aw - w - baseX), -baseX);
  view.y = h <= ah ? (ah - h) / 2 - baseY : Math.min(Math.max(view.y, ah - h - baseY), -baseY);
  // Snap the actual canvas origin. Keep fractional movement in view for panning.
  const dpr = window.devicePixelRatio;
  left = Math.round((baseX + view.x) * dpr) / dpr;
  top = Math.round((baseY + view.y) * dpr) / dpr;
  cv.style.transform = view.z === 1 && !left && !top
    ? '' : `translate(${left}px, ${top}px) scale(${view.z})`;
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
  view.x = px - baseX - at.x * view.z;
  view.y = py - baseY - at.y * view.z;
  applyView();
}

export function panBy(dx: number, dy: number) {
  view.x += dx;
  view.y += dy;
  applyView();
}

export const zoomed = () => view.z > 1;
