// Typing from a phone: the text field (the phone's own keyboard) and the
// on-screen PC keyboard. The layouts and the dock itself are in
// components/Keyboard.tsx.
import { createSignal } from 'solid-js';
import { createStore } from 'solid-js/store';
import { send } from './connection';

/** Which face of the dock is showing. The dock itself is always there on a
 *  touch screen; the phone's keyboard only opens when the field is tapped. */
export type Dock = 'text' | 'keys';
export const [dock, setDockSignal] = createSignal<Dock>('text');

// ---- the text field ----
// A draft: typed and corrected on the phone with its own keyboard, then sent
// to the host in one go. With the field empty, Enter and Backspace (and
// arrows, if the phone's keyboard has them) go to the host as keys.
export const field: { el: HTMLTextAreaElement | null } = { el: null };

export function tap(code: string) {
  send({ t: 'k', c: code, d: 1 });
  send({ t: 'k', c: code, d: 0 });
}

/** Send the draft and empty the field; with nothing in it, press Enter. */
export function sendDraft() {
  const el = field.el;
  if (el?.value) {
    send({ t: 'type', s: el.value });
    el.value = '';
  } else tap('Enter');
}

export function setDock(d: Dock) {
  if (d === 'keys') field.el?.blur();   // the PC keyboard replaces the phone's
  else releaseModifiers();
  setDockSignal(d);
}

// ---- modifiers ----
// Tap a modifier and it applies to the next key, then lets go; tap it again
// to let go without a key (so tapping Win twice opens the Start menu).
// Press and hold to lock it until it is tapped again. While active, the key
// is held down on the host, so Alt+Tab with Alt locked behaves.
export type ModState = 'latched' | 'locked';
export const [mods, setMods] = createStore<Record<string, ModState | undefined>>({});
export const MODIFIERS = new Set([
  'ShiftLeft', 'ShiftRight', 'ControlLeft', 'ControlRight', 'AltLeft', 'AltRight', 'MetaLeft', 'MetaRight',
]);
export const shifted = () => !!(mods.ShiftLeft || mods.ShiftRight);

export function tapModifier(code: string) {
  if (mods[code]) {
    send({ t: 'k', c: code, d: 0 });
    setMods(code, undefined);
  } else {
    send({ t: 'k', c: code, d: 1 });
    setMods(code, 'latched');
  }
}
export function lockModifier(code: string) {
  if (!mods[code]) send({ t: 'k', c: code, d: 1 });
  setMods(code, 'locked');
}
/** After a key: latched modifiers let go, locked ones stay. */
function releaseLatched() {
  for (const [code, state] of Object.entries(mods)) {
    if (state === 'latched') { send({ t: 'k', c: code, d: 0 }); setMods(code, undefined); }
  }
}
export function releaseModifiers() {
  for (const [code, state] of Object.entries(mods)) {
    if (state) { send({ t: 'k', c: code, d: 0 }); setMods(code, undefined); }
  }
}

// ---- keys ----
// Held keys repeat, as a real keyboard's would: the host does not repeat
// injected keys by itself.
const REPEAT_DELAY = 400, REPEAT_EVERY = 45;
let repeat: { code: string; timer: ReturnType<typeof setTimeout> } | null = null;

export function keyDown(code: string) {
  keyUp();
  send({ t: 'k', c: code, d: 1 });
  const again = () => {
    send({ t: 'k', c: code, d: 1 });
    if (repeat) repeat.timer = setTimeout(again, REPEAT_EVERY);
  };
  repeat = { code, timer: setTimeout(again, REPEAT_DELAY) };
}
export function keyUp() {
  if (!repeat) return;
  clearTimeout(repeat.timer);
  send({ t: 'k', c: repeat.code, d: 0 });
  repeat = null;
  releaseLatched();
}
