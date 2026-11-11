// Mouse, wheel, keyboard and clipboard, from this browser to the host.
import { release, setSleep, showSettings } from './actions';
import { hooks, send } from './connection';
import { cv, focusScreen } from './screen';
import { active, autoSlept, cfg, clipboardOn, isTouch, settingsOpen, sleeping, viewOnly } from './state';
import { clamp01 } from './util';
import { current, HOSTED } from './hosted';

// ---- mouse ----
function pos(e: PointerEvent): [number, number] {
  const r = cv.getBoundingClientRect();
  return [clamp01((e.clientX - r.left) / r.width), clamp01((e.clientY - r.top) / r.height)];
}
/** MouseEvent.buttons bit -> button index understood by the host. */
const BUTTON_BITS = [[1, 0], [2, 2], [4, 1], [8, 3], [16, 4]];
let held = 0;
let pendingMove: [number, number] | null = null, moveQueued = false;
/** A click that only closed the settings panel. */
let swallowing = false;

function flushMove() {
  moveQueued = false;
  if (pendingMove) { send({ t: 'm', x: pendingMove[0], y: pendingMove[1] }); pendingMove = null; }
}
/** Pointer moves go out at most once per animation frame. Positions are 0..1 of the screen. */
export function sendMove(x: number, y: number) {
  pendingMove = [x, y];
  if (!moveQueued) { moveQueued = true; requestAnimationFrame(flushMove); }
}
/** b: 0 left, 1 middle, 2 right. A press or release also places the pointer, so a pending move is dropped. */
export function sendButton(b: number, down: boolean, x: number, y: number) {
  pendingMove = null;
  send({ t: 'b', b, d: down ? 1 : 0, x, y });
}
// Diffing `buttons` (rather than trusting pointerdown/up) handles chorded
// clicks, where the second button only shows up as a pointermove.
function pointer(e: PointerEvent) {
  const [x, y] = pos(e);
  const now = e.type === 'pointerup' || e.type === 'pointercancel' ? e.buttons & held : e.buttons;
  if (now !== held) {
    pendingMove = null;
    for (const [bit, b] of BUTTON_BITS) {
      if ((now ^ held) & bit) send({ t: 'b', b, d: now & bit ? 1 : 0, x, y });
    }
    held = now;
  } else if (e.type === 'pointermove') {
    sendMove(x, y);
  }
}
// Fingers are handled in touch.ts; these are for the mouse and pen.
cv.addEventListener('pointerdown', e => {
  if (e.pointerType === 'touch') return;
  focusScreen();
  e.preventDefault();
  if (settingsOpen()) { swallowing = true; showSettings(false); return; }
  try { cv.setPointerCapture(e.pointerId); } catch {}
  pointer(e);
});
cv.addEventListener('pointermove', e => { if (e.pointerType !== 'touch' && !swallowing) pointer(e); });
cv.addEventListener('pointerup', e => {
  if (e.pointerType === 'touch') return;
  e.preventDefault();
  if (swallowing) { if (!e.buttons) swallowing = false; return; }
  pointer(e);
});
cv.addEventListener('pointercancel', e => {
  if (e.pointerType === 'touch') return;
  held = 0; swallowing = false; send({ t: 'rel' });
});
for (const type of ['contextmenu', 'mousedown', 'mouseup', 'auxclick', 'dragstart']) {
  cv.addEventListener(type, e => e.preventDefault());
}

let wheelX = 0, wheelY = 0;
cv.addEventListener('wheel', e => {
  e.preventDefault();
  // Normalise to Windows wheel units (120 per notch).
  const unit = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? 480 : 1.2;
  wheelX += e.deltaX * unit; wheelY -= e.deltaY * unit;
  const dx = Math.trunc(wheelX), dy = Math.trunc(wheelY);
  if (dx || dy) { wheelX -= dx; wheelY -= dy; send({ t: 'w', dx, dy }); }
}, { passive: false });

// ---- clipboard ----
// Host -> client: the host tells us when its clipboard text changes.
// Client -> host: on Ctrl+V we let the browser raise its own `paste` event,
// which is the one way to read the local clipboard on a plain-HTTP page,
// send that text to the host, and only then press V there.
let lastClip: string | null = null;
const clipbuf = document.createElement('textarea');
clipbuf.id = 'clipbuf';
clipbuf.tabIndex = -1;
clipbuf.setAttribute('aria-hidden', 'true');
document.body.append(clipbuf);

hooks.clip = text => {
  lastClip = text;
  if (window.isSecureContext && navigator.clipboard) {
    navigator.clipboard.writeText(text).catch(() => copyViaSelection(text));
  } else if (!isTouch()) copyViaSelection(text);
  // (On a touch screen, selecting text in a box would bring up the phone's keyboard.)
};
// Works on plain HTTP, but only shortly after a key press or click, which a
// copy on the remote always is.
function copyViaSelection(text: string) {
  clipbuf.value = text;
  clipbuf.select();
  try { document.execCommand('copy'); } catch {}
  clipbuf.value = '';
  focusScreen();
}

/** ms to wait for the browser's paste event. */
const PASTE_WAIT = 150;
let pasteTimer: ReturnType<typeof setTimeout> | null = null, swallowV = false;
function pressV() { send({ t: 'k', c: 'KeyV', d: 1 }); send({ t: 'k', c: 'KeyV', d: 0 }); }
/** Typing in this page's own text fields (the keyboard bar's, the machine
 *  list's) is not for the host. */
const ownField = (e: Event) => {
  const t = e.target as Element | null;
  return !!t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA') && t.id !== 'clipbuf';
};
document.addEventListener('paste', e => {
  if (ownField(e)) return;
  e.preventDefault();
  if (!pasteTimer) return;                       // not a paste we are waiting for
  clearTimeout(pasteTimer); pasteTimer = null;
  const text = e.clipboardData ? e.clipboardData.getData('text/plain') : '';
  if (text && text !== lastClip) { lastClip = text; send({ t: 'clip', s: text }); }
  pressV();
});

// ---- keyboard ----
// The Windows key only goes to the host in fullscreen; otherwise the client's
// own Start menu opens too and both machines react.
const META = new Set(['MetaLeft', 'MetaRight']);
/** A key handled here (wake, close settings); its release stays here too. */
let swallowKey: string | null = null;
function key(e: KeyboardEvent, down: boolean) {
  if (!e.code || e.code === 'Unidentified' || ownField(e) || (HOSTED && !current())) return;
  if (!down && e.code === swallowKey) { swallowKey = null; e.preventDefault(); return; }
  if (down && settingsOpen() && e.code === 'Escape') {
    e.preventDefault(); swallowKey = e.code; showSettings(false); return;
  }
  if (sleeping()) {
    // Nothing goes to the host while asleep; browser shortcuts keep working.
    if (down && (e.code === 'Space' || e.code === 'Enter')) {
      e.preventDefault(); swallowKey = e.code; setSleep(false);
    }
    return;
  }
  if (e.code === 'KeyV' && clipboardOn() && !viewOnly()) {
    if (down && e.ctrlKey && !e.altKey) {
      // Not prevented, so the browser fires `paste`; if it does not (empty
      // or non-text clipboard), the timer presses V anyway.
      swallowV = true;
      if (!pasteTimer) pasteTimer = setTimeout(() => { pasteTimer = null; pressV(); }, PASTE_WAIT);
      return;
    }
    if (!down && swallowV) { swallowV = false; e.preventDefault(); return; }
  }
  e.preventDefault();
  if (META.has(e.code) && !document.fullscreenElement) return;
  send({ t: 'k', c: e.code, d: down ? 1 : 0 });
}
addEventListener('keydown', e => key(e, true), true);
addEventListener('keyup', e => key(e, false), true);

// ---- focus ----
release.all = () => { held = 0; swallowV = false; send({ t: 'rel' }); };
addEventListener('blur', () => {
  release.all();
  if (cfg.autoSleep && !sleeping()) setSleep(true, true);
});
// Only an automatic pause ends on its own; one the user chose stays.
addEventListener('focus', () => { if (sleeping() && autoSlept()) setSleep(false); });
document.addEventListener('visibilitychange', () => {
  if (document.hidden) release.all();
  send({ t: 'vis', v: active() });
});
