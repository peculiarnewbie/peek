import { For, Show } from 'solid-js';
import { applyCfg } from '../actions';
import { focusScreen } from '../screen';
import { cfg, FPS_STEPS, monitors, QUALITY, settingsOpen, viewOnly, type Cfg } from '../state';
import { PairDevice } from './PairDevice';

const PRESETS: [string, Partial<Cfg>][] = [
  ['Saver', { quality: 'low', scale: 1, soft: true, fps: 30 }],
  ['Balanced', { quality: 'med', scale: 1, soft: false, fps: 60 }],
  ['Crisp', { quality: 'high', scale: 1, soft: false, fps: 60 }],
];

interface Control<K extends keyof Cfg = keyof Cfg> {
  key: K;
  label: string;
  options: () => [Cfg[K], string][];
  value: () => string;
  hidden?: () => boolean;
}
// One entry per setting; each is applied as soon as it is picked.
const CONTROLS: Control[] = [
  { key: 'quality', label: 'Quality',
    options: () => [['lowest', 'Lowest'], ['low', 'Low'], ['med', 'Medium'], ['high', 'High']],
    value: () => `JPEG ${QUALITY[cfg.quality][0]} → ${QUALITY[cfg.quality][1]}` },
  { key: 'scale', label: 'Resolution', options: () => [[1, 'Full'], [2, 'Half']],
    value: () => {
      const m = monitors()[cfg.mon];
      return m ? `${Math.ceil(m.w / cfg.scale)} × ${Math.ceil(m.h / cfg.scale)}` : '';
    } },
  { key: 'soft', label: 'Motion', options: () => [[false, 'Sharp'], [true, 'Soft']], value: () => '' },
  { key: 'lossless', label: 'Lossless text', options: () => [[false, 'Off'], [true, 'On']],
    value: () => cfg.lossless ? 'photos stay JPEG' : '' },
  { key: 'fps', label: 'Frame rate', options: () => FPS_STEPS.map(f => [f, String(f)]), value: () => '' },
  { key: 'mon', label: 'Monitor', options: () => monitors().map((_, i) => [i, String(i + 1)]),
    value: () => { const m = monitors()[cfg.mon]; return m ? `${m.w} × ${m.h}` : ''; },
    hidden: () => monitors().length < 2 },
  { key: 'autoSleep', label: 'Pause when unfocused', options: () => [[false, 'Off'], [true, 'On']], value: () => '' },
];

function Segmented<T>(props: { options: [T, string][]; selected: (v: T) => boolean; pick: (v: T) => void }) {
  return (
    <div class="seg" role="radiogroup">
      <For each={props.options}>{([value, text]) =>
        <button tabIndex={-1} role="radio" aria-checked={props.selected(value)}
          onClick={() => { props.pick(value); focusScreen(); }}>{text}</button>
      }</For>
    </div>
  );
}

function Group(props: { label: string; value: string; children: any }) {
  return (
    <div class="group">
      <div class="label"><span>{props.label}</span><span class="val">{props.value}</span></div>
      {props.children}
    </div>
  );
}

export function Settings() {
  const preset = () => PRESETS.find(([, p]) =>
    (Object.keys(p) as (keyof Cfg)[]).every(k => p[k] === cfg[k]));
  return (
    <div id="settings" class="panel" classList={{ show: settingsOpen() }} role="dialog" aria-label="Stream settings">
      <Group label="Preset" value={preset() ? '' : 'custom'}>
        <Segmented options={PRESETS.map(([name]) => [name, name])}
          selected={name => preset()?.[0] === name}
          pick={name => applyCfg(PRESETS.find(([n]) => n === name)![1])} />
      </Group>
      <For each={CONTROLS}>{c =>
        <Show when={!c.hidden?.()}>
          <Group label={c.label} value={c.value()}>
            <Segmented options={c.options()} selected={v => cfg[c.key] === v}
              pick={v => applyCfg({ [c.key]: v })} />
          </Group>
        </Show>
      }</For>
      <Show when={!viewOnly()}>
        <Group label="Pair a device" value="">
          <PairDevice />
        </Group>
      </Show>
    </div>
  );
}
