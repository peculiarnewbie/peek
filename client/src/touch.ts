// Fingers on the remote screen.
//
// Trackpad mode (default), as on a laptop: one finger moves the cursor,
// tap clicks, two-finger tap right-clicks, three-finger tap middle-clicks,
// tap then touch again and drag (or long-press and drag) holds the button.
// Direct mode: the finger is the pointer. Tap clicks there, long-press
// right-clicks, drag drags.
// Both: two fingers moving together scroll; pinching zooms the view. In
// trackpad mode a zoomed view keeps the cursor in the middle of the window,
// except near the screen's edges, where the view stops and the cursor
// carries on to the edge. In direct mode the pinch zooms around the fingers
// and pans with them.
import { showSettings } from './actions';
import { hooks, send } from './connection';
import { sendButton, sendMove } from './input';
import { areaH, areaW, onViewChange, shownRect, stage, toCanvas, view, zoomed, zoomTo } from './screen';
import { setBarOpen, setIsTouch, settingsOpen, touchMode } from './state';
import { clamp01 } from './util';

/** CSS px a finger may wander and still count as a tap or a press. */
const SLOP = 8;
const LONG_PRESS_MS = 450;
/** Trackpad: touching again this soon after a tap, then moving, drags. */
const TAP_DRAG_MS = 300;
/** Two- and three-finger taps must be at least this quick. */
const MULTI_TAP_MS = 350;
/** Windows wheel units (120 a notch) per CSS px of two-finger movement. */
const WHEEL_PER_PX = 3;

// ---- the cursor ----
// The host's screen capture has no pointer in it. With a mouse the browser
// draws one; on a touch screen there is none, so draw it here.
export const cursor = { x: 0.5, y: 0.5 };
const ARROW = '<svg width="14" height="21" viewBox="-1 -1 14 21"><path d="M0 0v17l4.5-4.2 2.9 6.7 2.6-1.1-2.8-6.6H13z" fill="#fff" stroke="#000" stroke-width="1.2" stroke-linejoin="round"/></svg>';
const IBEAM = '<svg width="10" height="20" viewBox="-5 -10 10 20" style="margin:-10px 0 0 -5px"><path d="M-3-9h6M0-9v18M-3 9h6" stroke="#000" stroke-width="3.2" stroke-linecap="round"/><path d="M-3-9h6M0-9v18M-3 9h6" stroke="#fff" stroke-width="1.4" stroke-linecap="round"/></svg>';
const cursorEl = document.createElement('div');
cursorEl.id = 'cursor';
cursorEl.innerHTML = ARROW;
document.body.append(cursorEl);

hooks.cursor = css => {
  cursorEl.innerHTML = css === 'text' ? IBEAM : ARROW;
  cursorEl.style.visibility = css === 'none' ? 'hidden' : '';
};

function cursorOnPage() {
  const r = shownRect();
  return { x: r.left + cursor.x * r.width, y: r.top + cursor.y * r.height };
}
function placeCursor() {
  const p = cursorOnPage();
  cursorEl.style.transform = `translate(${p.x}px, ${p.y}px)`;
}
onViewChange(placeCursor);

/** Trackpad: zoom to `z` with the cursor in the middle of the window, as
 *  far as the screen's edges allow (the view never shows past them). */
function centerOnCursor(z: number) {
  const r = shownRect();
  const at = { x: (cursor.x * r.width) / view.z, y: (cursor.y * r.height) / view.z };
  zoomTo(z, at, areaW() / 2, areaH() / 2);
}
function followCursor() {
  if (zoomed()) centerOnCursor(view.z);
}

/** Trackpad: move by a finger's movement in CSS px, so it tracks the finger at any zoom. */
function moveCursorBy(dx: number, dy: number) {
  const r = shownRect();
  cursor.x = clamp01(cursor.x + dx / r.width);
  cursor.y = clamp01(cursor.y + dy / r.height);
  sendMove(cursor.x, cursor.y);
  followCursor();
  placeCursor();
}
/** Direct: put the cursor under a point on the page. */
function cursorTo(px: number, py: number) {
  const r = shownRect();
  cursor.x = clamp01((px - r.left) / r.width);
  cursor.y = clamp01((py - r.top) / r.height);
  sendMove(cursor.x, cursor.y);
  placeCursor();
}

function click(b: number) {
  sendButton(b, true, cursor.x, cursor.y);
  sendButton(b, false, cursor.x, cursor.y);
}

// ---- gestures ----
interface Point { x: number; y: number }
interface Finger extends Point { x0: number; y0: number; t: number }
const fingers = new Map<number, Finger>();

type Kind =
  | 'pending'    // one finger down, not yet a tap, move or press
  | 'pending2'   // two or more fingers, not yet a tap, scroll or pinch
  | 'move' | 'drag' | 'scroll' | 'pinch'
  | 'ignore';    // finished; waiting for the remaining fingers to lift
interface Gesture {
  kind: Kind;
  /** Most fingers down at once. */
  most: number;
  t0: number;
  /** Trackpad: began just after a tap, so moving drags. */
  afterTap: boolean;
  timer?: ReturnType<typeof setTimeout>;
  // Two fingers: where they started, and the zoom and canvas point under them then.
  mid0?: Point; dist0?: number; z0?: number; at?: Point; last?: Point;
}
let g: Gesture | null = null;
/** When the last trackpad tap ended. */
let lastTap = 0;
let dragging = false;
const wheel = { x: 0, y: 0 };

const two = () => [...fingers.values()].slice(0, 2);
const mid = (): Point => { const [a, b] = two(); return { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 }; };
const spread = () => { const [a, b] = two(); return Math.hypot(a.x - b.x, a.y - b.y); };

function startTwo(gest: Gesture) {
  const m = mid();
  Object.assign(gest, { kind: 'pending2', mid0: m, last: m, dist0: Math.max(spread(), 1), z0: view.z, at: toCanvas(m.x, m.y) });
}

function startDrag(gest: Gesture, from: Finger) {
  if (touchMode() === 'direct') cursorTo(from.x0, from.y0);
  gest.kind = 'drag';
  dragging = true;
  sendButton(0, true, cursor.x, cursor.y);
  if (touchMode() === 'direct') cursorTo(from.x, from.y);
}
function endDrag() {
  if (!dragging) return;
  dragging = false;
  sendButton(0, false, cursor.x, cursor.y);
}

function longPress() {
  if (!g || g.kind !== 'pending' || fingers.size !== 1) return;
  const f = [...fingers.values()][0];
  navigator.vibrate?.(15);
  if (touchMode() === 'trackpad') startDrag(g, f);
  else { cursorTo(f.x0, f.y0); click(2); g.kind = 'ignore'; }
}

/** Faster finger movement moves the cursor further, as a mouse does. */
function accel(f: Finger, dx: number, dy: number, now: number) {
  const speed = Math.hypot(dx, dy) / Math.max(now - f.t, 1);   // px per ms
  return 1 + Math.min(speed, 2) * 0.75;
}

// On the whole stage, so the bars beside or above a letterboxed screen work
// as trackpad too.
stage.addEventListener('pointerdown', e => {
  if (e.pointerType !== 'touch') return;
  e.preventDefault();
  setIsTouch(true);
  setBarOpen(false);
  const now = performance.now();
  fingers.set(e.pointerId, { x: e.clientX, y: e.clientY, x0: e.clientX, y0: e.clientY, t: now });
  if (fingers.size === 1) {
    if (settingsOpen()) {
      // A tap outside the settings panel only closes it.
      showSettings(false);
      g = { kind: 'ignore', most: 1, t0: now, afterTap: false };
      return;
    }
    g = { kind: 'pending', most: 1, t0: now, afterTap: touchMode() === 'trackpad' && now - lastTap < TAP_DRAG_MS };
    g.timer = setTimeout(longPress, LONG_PRESS_MS);
    if (touchMode() === 'direct') cursorTo(e.clientX, e.clientY);
    return;
  }
  if (!g) return;
  g.most = Math.max(g.most, fingers.size);
  clearTimeout(g.timer);
  if (fingers.size === 2 && (g.kind === 'pending' || g.kind === 'move')) startTwo(g);
});

stage.addEventListener('pointermove', e => {
  if (e.pointerType !== 'touch') return;
  const f = fingers.get(e.pointerId);
  if (!f || !g) return;
  e.preventDefault();
  const now = performance.now();
  const dx = e.clientX - f.x, dy = e.clientY - f.y;
  f.x = e.clientX; f.y = e.clientY;
  const trackpad = touchMode() === 'trackpad';

  if (g.kind === 'pending' && Math.hypot(f.x - f.x0, f.y - f.y0) > SLOP) {
    clearTimeout(g.timer);
    if (trackpad) {
      if (g.afterTap) startDrag(g, f); else g.kind = 'move';
      moveCursorBy(f.x - f.x0, f.y - f.y0);   // the movement within the slop counts too
    } else startDrag(g, f);
  } else if (g.kind === 'move' || g.kind === 'drag') {
    if (!trackpad) cursorTo(f.x, f.y);
    else { const k = accel(f, dx, dy, now); moveCursorBy(dx * k, dy * k); }
  } else if (g.kind === 'pending2' && fingers.size >= 2) {
    const m = mid();
    const moved = Math.hypot(m.x - g.mid0!.x, m.y - g.mid0!.y);
    const pinched = Math.abs(spread() - g.dist0!);
    if (Math.max(moved, pinched) > SLOP) g.kind = pinched > moved ? 'pinch' : 'scroll';
  }

  if (g.kind === 'pinch' && fingers.size >= 2) {
    const z = g.z0! * spread() / g.dist0!;
    if (trackpad) centerOnCursor(z);
    else { const m = mid(); zoomTo(z, g.at!, m.x, m.y); }
  } else if (g.kind === 'scroll' && fingers.size >= 2) {
    // Content follows the fingers, as when scrolling a page on the phone.
    const m = mid();
    wheel.x -= (m.x - g.last!.x) * WHEEL_PER_PX;
    wheel.y += (m.y - g.last!.y) * WHEEL_PER_PX;
    g.last = m;
    const wx = Math.trunc(wheel.x), wy = Math.trunc(wheel.y);
    if (wx || wy) { wheel.x -= wx; wheel.y -= wy; send({ t: 'w', dx: wx, dy: wy }); }
  }
  f.t = now;
});

function lift(e: PointerEvent, cancelled: boolean) {
  if (e.pointerType !== 'touch') return;
  const f = fingers.get(e.pointerId);
  if (!f) return;
  fingers.delete(e.pointerId);
  e.preventDefault();
  if (!g) return;
  clearTimeout(g.timer);
  if (g.kind === 'drag') { endDrag(); g.kind = 'ignore'; }
  if (fingers.size) {
    // One of several fingers lifted: a scroll or pinch ends, a multi-finger tap waits for the rest.
    if (g.kind !== 'pending2') g.kind = 'ignore';
    return;
  }
  const now = performance.now();
  if (!cancelled && g.kind === 'pending') {
    if (touchMode() === 'trackpad') { click(0); lastTap = now; }
    else { cursorTo(f.x0, f.y0); click(0); }
  } else if (!cancelled && g.kind === 'pending2' && now - g.t0 < MULTI_TAP_MS) {
    click(g.most >= 3 ? 1 : 2);
  }
  wheel.x = wheel.y = 0;
  g = null;
}
stage.addEventListener('pointerup', e => lift(e, false));
stage.addEventListener('pointercancel', e => lift(e, true));
stage.addEventListener('contextmenu', e => e.preventDefault());
