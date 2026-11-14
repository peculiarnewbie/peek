import { SCREEN_PLUGIN } from '../plugin-mode';
import { createSignal, For, Show } from 'solid-js';
import { applyCfg, showSettings } from '../actions';
import { focusScreen } from '../screen';
import { cfg, FPS_STEPS, monitors, QUALITY, settingsOpen, viewOnly, type Cfg } from '../state';
import { machine } from '../connection';
import { current, HOSTED, learn, type About } from '../hosted';
import { upgrade, updating } from '../update';
import { own, setOwn } from '../views';
import { hasPlugins, openPage, PLUGINS } from './Frame';
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

/** The peek this machine runs, and an update to a newer one. */
function Version() {
  const about = () => HOSTED ? current() : own();
  const base = () => machine()?.base ?? '';
  const [error, setError] = createSignal('');
  const go = () => {
    const m = machine();
    if (!m) return;
    setError('');
    upgrade(m.base, m.token, a => {
      const shown = current();
      if (HOSTED && shown) learn(shown, a as unknown as About);
      else setOwn(a as ReturnType<typeof own>);
    }).catch(e => setError(`Not updated: ${e?.message ?? e}`));
  };
  return (
    <Show when={about()?.version}>
      <Group label="peek" value={about()!.version!}>
        <Show when={updating[base()] || (about()?.update && !viewOnly())}>
          <button class="btn wide alt" tabIndex={-1} disabled={!!updating[base()]} onClick={go}>
            {updating[base()] ?? `Update to ${about()!.update}`}
          </button>
        </Show>
        <Show when={error()}><p class="vtext wrong" style={{ 'margin-top': '8px' }}>{error()}</p></Show>
      </Group>
    </Show>
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
      <Show when={!SCREEN_PLUGIN && hasPlugins()}>
        <Group label="Plugins" value="">
          <button class="btn wide alt" tabIndex={-1} onClick={() => { showSettings(false); openPage(PLUGINS); }}>
            Manage plugins
          </button>
        </Group>
      </Show>
      <Show when={!SCREEN_PLUGIN}>
        <Version />
      </Show>
      <Show when={!SCREEN_PLUGIN && !viewOnly()}>
        <Group label="Pair a device" value="">
          <PairDevice />
        </Group>
      </Show>
    </div>
  );
}
