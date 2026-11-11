// The keyboard dock at the bottom of the screen, for phones and tablets.
// Two faces: a text field that types through the phone's own keyboard, and
// a PC keyboard (with a Shift layer and an Fn layer of F-keys, numpad and
// navigation keys) for everything a phone keyboard lacks.
import { createSignal, For, onCleanup, onMount } from 'solid-js';
import {
  dock, field, keyDown, keyUp, lockModifier, mods, resetText, setDock, shifted, syncText, tap, tapModifier,
} from '../keyboard';
import { setBottomInset } from '../screen';
import { Icon } from './Icons';

type Kind = 'key' | 'mod' | 'fn' | 'text' | 'hide' | 'gap';
interface KeyDef { code: string; label: string; shift?: string; w: number; kind: Kind; small?: boolean }

const key = (code: string, label: string, shift?: string, w = 1): KeyDef => ({ code, label, shift, w, kind: 'key' });
const wide = (code: string, label: string, w: number): KeyDef => ({ code, label, w, kind: 'key', small: label.length > 2 });
const small = (code: string, label: string, w = 1): KeyDef => ({ code, label, w, kind: 'key', small: true });
const mod = (code: string, label: string, w = 1): KeyDef => ({ code, label, w, kind: 'mod', small: label.length > 2 });
const gap = (w = 0.4): KeyDef => ({ code: '', label: '', w, kind: 'gap' });
const special = (kind: Kind, label: string, w = 1): KeyDef => ({ code: '', label, w, kind, small: label.length > 2 });
const letters = (s: string) => [...s].map(c => key('Key' + c.toUpperCase(), c, c.toUpperCase()));
const pairs = (codes: string[], plain: string, shift: string) => codes.map((c, i) => key(c, plain[i], shift[i]));
const DIGITS = [...'1234567890'].map(d => 'Digit' + d);

const BOTTOM_RIGHT = [mod('AltRight', 'Alt'), mod('MetaRight', 'Win'), small('ContextMenu', 'Menu'),
  mod('ControlRight', 'Ctrl'), special('text', '✎'), special('hide', '▾')];

const MAIN: KeyDef[][] = [
  [key('Backquote', '`', '~'), ...pairs(DIGITS, '1234567890', '!@#$%^&*()'), key('Minus', '-', '_'),
    key('Equal', '=', '+'), wide('Backspace', '⌫', 1.5)],
  [wide('Tab', 'Tab', 1.5), ...letters('qwertyuiop'), key('BracketLeft', '[', '{'), key('BracketRight', ']', '}'),
    key('Backslash', '\\', '|')],
  [wide('CapsLock', 'Caps', 1.75), ...letters('asdfghjkl'), key('Semicolon', ';', ':'), key('Quote', "'", '"'),
    wide('Enter', '↵', 1.75)],
  [mod('ShiftLeft', '⇧', 2.25), ...letters('zxcvbnm'), key('Comma', ',', '<'), key('Period', '.', '>'),
    key('Slash', '/', '?'), mod('ShiftRight', '⇧', 2.25)],
  [special('fn', 'Fn'), mod('ControlLeft', 'Ctrl'), mod('MetaLeft', 'Win'), mod('AltLeft', 'Alt'),
    key('Space', '', undefined, 4.5), ...BOTTOM_RIGHT],
];

const F = (n: number) => small('F' + n, 'F' + n);
const NUM = (d: string) => key('Numpad' + d, d);
const blank = (w = 1) => gap(w);
// Three blocks, as on a full-size keyboard: F-keys, numpad, navigation.
const FN: KeyDef[][] = [
  [small('Escape', 'Esc'), F(1), F(2), F(3), F(4), gap(),
    small('NumLock', 'Num'), key('NumpadDivide', '/'), key('NumpadMultiply', '*'), key('NumpadSubtract', '-'), gap(),
    small('Insert', 'Ins'), small('Home', 'Home'), small('PageUp', 'PgUp'), key('Backspace', '⌫')],
  [small('Tab', 'Tab'), F(5), F(6), F(7), F(8), gap(),
    NUM('7'), NUM('8'), NUM('9'), key('NumpadAdd', '+'), gap(),
    small('Delete', 'Del'), small('End', 'End'), small('PageDown', 'PgDn'), blank()],
  [blank(), F(9), F(10), F(11), F(12), gap(),
    NUM('4'), NUM('5'), NUM('6'), blank(), gap(),
    blank(), key('ArrowUp', '↑'), blank(), blank()],
  [mod('ShiftLeft', '⇧'), small('PrintScreen', 'PrtSc'), small('ScrollLock', 'ScrLk'), small('Pause', 'Pause'), blank(), gap(),
    NUM('1'), NUM('2'), NUM('3'), key('NumpadEnter', '↵'), gap(),
    key('ArrowLeft', '←'), key('ArrowDown', '↓'), key('ArrowRight', '→'), mod('ShiftRight', '⇧')],
  [special('fn', 'Fn'), mod('ControlLeft', 'Ctrl'), mod('MetaLeft', 'Win'), mod('AltLeft', 'Alt'), key('Space', ''), gap(),
    key('Numpad0', '0', undefined, 2), key('NumpadDecimal', '.'), blank(), gap(),
    mod('ControlRight', 'Ctrl'), mod('AltRight', 'Alt'), special('text', '✎'), special('hide', '▾')],
];

const LONG_PRESS_MS = 450;

function Key(props: { def: KeyDef; fn: boolean; toggleFn: () => void }) {
  const d = props.def;
  const [down, setDown] = createSignal(false);
  let lockTimer: ReturnType<typeof setTimeout> | undefined, locked = false;

  const press = (e: PointerEvent) => {
    e.preventDefault();   // keep focus (and the phone's keyboard) where it is
    (e.currentTarget as Element).releasePointerCapture?.(e.pointerId);
    setDown(true);
    if (d.kind === 'key') keyDown(d.code);
    else if (d.kind === 'mod') {
      locked = false;
      lockTimer = setTimeout(() => { locked = true; navigator.vibrate?.(15); lockModifier(d.code); }, LONG_PRESS_MS);
    }
  };
  const release = (e: PointerEvent) => {
    if (!down()) return;
    setDown(false);
    if (d.kind === 'key') keyUp();
    else if (d.kind === 'mod') { clearTimeout(lockTimer); if (!locked && e.type === 'pointerup') tapModifier(d.code); }
    else if (e.type !== 'pointerup') return;
    else if (d.kind === 'fn') props.toggleFn();
    else if (d.kind === 'text') setDock('text');
    else if (d.kind === 'hide') setDock('off');
  };
  const label = () => (shifted() && d.shift) || d.label;
  return d.kind === 'gap'
    ? <div class="key gap" style={{ 'flex-grow': d.w }} />
    : <button class="key" tabIndex={-1} style={{ 'flex-grow': d.w }}
        classList={{
          down: down(), small: !!d.small, mod: d.kind === 'mod', space: d.code === 'Space',
          latched: d.kind === 'mod' && mods[d.code] === 'latched',
          locked: d.kind === 'mod' && mods[d.code] === 'locked',
          on: d.kind === 'fn' && props.fn,
        }}
        onPointerDown={press} onPointerUp={release} onPointerCancel={release} onPointerLeave={release}
        onContextMenu={e => e.preventDefault()}>{label()}</button>;
}

export function Keyboard() {
  let box!: HTMLDivElement;
  const [fn, setFn] = createSignal(false);

  // The remote screen is fitted into the space above the dock.
  onMount(() => {
    const ro = new ResizeObserver(() => setBottomInset(box.offsetHeight));
    ro.observe(box);
    onCleanup(() => ro.disconnect());
  });

  // Keys the phone's keyboard does send as keys (Enter, and Backspace or
  // arrows on an empty field) go to the host as keys.
  const onKeyDown = (e: KeyboardEvent) => {
    e.stopPropagation();
    const empty = !(e.currentTarget as HTMLInputElement).value;
    if (e.key === 'Enter') { e.preventDefault(); tap('Enter'); resetText(); }
    else if (empty && e.code && e.key.length > 1 && e.key !== 'Unidentified' && e.key !== 'Process') {
      e.preventDefault();
      tap(e.code);
    }
  };

  // ⌫ in the bar: takes a character off the field if there is one (which
  // deletes it on the host), else presses Backspace. Repeats while held.
  let repeat: ReturnType<typeof setTimeout> | undefined;
  const backspace = () => {
    const el = field.el;
    if (el && el.value) { el.value = [...el.value].slice(0, -1).join(''); syncText(el.value); }
    else tap('Backspace');
  };
  const holdBackspace = (e: PointerEvent) => {
    e.preventDefault();
    backspace();
    const again = (wait: number) => { repeat = setTimeout(() => { backspace(); again(60); }, wait); };
    again(400);
  };
  const stopBackspace = () => clearTimeout(repeat);
  const keep = (e: PointerEvent) => e.preventDefault();   // buttons must not take focus from the field

  return (
    <div id="kbd" ref={box} classList={{ show: dock() !== 'off', keys: dock() === 'keys' }}>
      <div class="kbd-bar">
        <button class="kbd-btn" tabIndex={-1} title="PC keyboard: Esc, Ctrl, F-keys, arrows…" onPointerDown={keep}
          onClick={() => setDock('keys')}><Icon name="keyboard" /></button>
        <input id="kbd-text" ref={el => { field.el = el; }} placeholder="Send text" autocomplete="off"
          autocapitalize="off" enterkeyhint="send"
          onInput={e => syncText(e.currentTarget.value)} onKeyDown={onKeyDown} />
        <button class="kbd-btn" tabIndex={-1} title="Backspace" onPointerDown={holdBackspace}
          onPointerUp={stopBackspace} onPointerCancel={stopBackspace} onPointerLeave={stopBackspace}>
          <Icon name="backspace" /></button>
        <button class="kbd-btn" tabIndex={-1} title="Enter" onPointerDown={keep}
          onClick={() => { tap('Enter'); resetText(); }}><Icon name="enter" /></button>
        <button class="kbd-btn" tabIndex={-1} title="Hide" onPointerDown={keep}
          onClick={() => setDock('off')}><Icon name="x" /></button>
      </div>
      <div class="kbd-keys">
        <For each={fn() ? FN : MAIN}>{row =>
          <div class="kbd-row">
            <For each={row}>{d => <Key def={d} fn={fn()} toggleFn={() => setFn(!fn())} />}</For>
          </div>
        }</For>
      </div>
    </div>
  );
}
