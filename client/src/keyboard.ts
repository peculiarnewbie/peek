// Typing from a phone: the text field (the phone's own keyboard) and the
// on-screen PC keyboard. The layouts and the dock itself are in
// components/Keyboard.tsx.
import { createSignal } from 'solid-js';
import { createStore } from 'solid-js/store';
import { send } from './connection';

/** Which part of the dock is showing. */
export type Dock = 'off' | 'text' | 'keys';
export const [dock, setDockSignal] = createSignal<Dock>('off');

// ---- the text field ----
// Phone keyboards (Gboard and the like) do not report keys: they edit the
// field, with autocorrect, swipe typing and suggestions. So we watch the
// field's text and make the host's match it: whatever changed since the
// last edit is deleted with Backspace and typed again as characters. A
// correction that turns "teh " into "the " sends two Backspaces and "he ".

/** The part of the field the host has typed. */
let sent = '';
export const field: { el: HTMLInputElement | null } = { el: null };

export function tap(code: string) {
  send({ t: 'k', c: code, d: 1 });
  send({ t: 'k', c: code, d: 0 });
}

export function syncText(value: string) {
  let i = 0;
  while (i < sent.length && i < value.length && sent[i] === value[i]) i++;
  // Never split a surrogate pair: back up to the start of the character.
  if (i > 0 && /[\uD800-\uDBFF]/.test(sent[i - 1])) i--;
  // One Backspace per character (code point) removed.
  for (let n = [...sent.slice(i)].length; n > 0; n--) tap('Backspace');
  const added = value.slice(i);
  if (added) send({ t: 'type', s: added });
  sent = value;
}

/** Start over with an empty field, e.g. after Enter or when the host's caret may have moved. */
export function resetText() {
  sent = '';
  if (field.el) field.el.value = '';
}

export function setDock(d: Dock) {
  if (dock() !== 'keys' && d === 'keys') resetText();
  if (d !== 'keys') releaseModifiers();
  setDockSignal(d);
  // Focusing the field is what brings up the phone's keyboard. It has to
  // happen inside the tap, so no waiting for anything to render.
  if (d === 'text') field.el?.focus({ preventScroll: true });
  else { field.el?.blur(); resetText(); }
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
