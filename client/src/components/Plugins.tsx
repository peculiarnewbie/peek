// Hosted client: every paired machine's plugins side by side, at `/plugins`
// (plugins.ts asks each machine). Two layouts of the same thing: a grid of
// machines by plugins, and one row per plugin with the machines it is on.
// Turning a plugin on or off, installing it where another machine has it,
// and updating an older copy are done from here; anything else is on the
// machine's own Plugins page, which a machine's name opens.
import { createMemo, For, onSettled, Show } from 'solid-js';
import type { JSX } from '@solidjs/web';
import { machines, navigate, pagePath, reach, type Machine } from '../hosted';
import {
  ago, changing, columns, fetchTo, layout, offers, OWN, pluginError, pluginState, portable, refreshAllPlugins, setLayout,
  setPluginError, turn, type Column, type Offer, type PluginItem,
} from '../plugins';

const openPlugins = (m: Machine) => navigate(pagePath(m, 'plugins'));
const itemOf = (m: Machine, name: string) => m.plugins?.items.find(i => i.name === name);
const busy = (m: Machine, name: string) => !!changing[`${m.id}/${name}`];
const away = (m: Machine) => reach[m.id] === 'down';
const sourceWord = (c: Column) => c.kind === OWN ? 'built in' : c.kind === 'a command' ? 'command'
  : !c.latest?.from ? 'plugin' : c.latest.from.startsWith('https://') ? 'git' : portable(c.latest.from) ? 'npm' : 'folder';

/** Why a machine has nothing to show, if it has not. */
function emptyWhy(m: Machine): string | null {
  if (pluginState[m.id] === 'old') return 'Update peek there to see its plugins here';
  if (!m.plugins) return reach[m.id] === 'down' ? 'Offline' : pluginState[m.id] === 'loading' ? 'Asking…' : 'Not known yet';
  return null;
}

export function Switch(props: { on: boolean; disabled?: boolean; busy?: boolean; stale?: boolean; title: string; onToggle: () => void }) {
  return (
    <button class={['psw', { on: props.on, stale: props.stale, busy: props.busy }]} role="switch"
      aria-checked={props.on ? 'true' : 'false'} title={props.title} disabled={props.disabled || props.busy} tabindex={-1}
      onClick={e => { e.stopPropagation(); props.onToggle(); }} />
  );
}

/** The button for installing or updating a plugin on a machine. */
function FetchButton(props: { m: Machine; c: Column; offer: Offer; long?: boolean }): JSX.Element {
  const o = () => props.offer.t === 'toggle' ? null : props.offer;
  const label = () => {
    const f = o();
    if (!f) return '';
    if (busy(props.m, props.c.name)) return 'Working…';
    if (f.t === 'install') return props.long ? `Install on ${props.m.name}` : 'Install';
    return props.long ? `Update ${props.m.name} to ${f.version}` : `Update to ${f.version}`;
  };
  return (
    <Show when={o()}>{f => (
      <button class={props.long ? 'btn small alt' : 'pbtn'} tabindex={-1} disabled={busy(props.m, props.c.name)}
        title={`From ${f().source}`} onClick={e => { e.stopPropagation(); void fetchTo(props.m, props.c.name, f().source, f().t === 'update'); }}>
        {label()}
      </button>
    )}</Show>
  );
}

// ---- grid: machines down, plugins across ----

function Grid() {
  return (
    <div class="panel pgrid-wrap">
      <table class="pgrid">
        <thead>
          <tr>
            <th>Machine</th>
            <For each={columns()}>{c => {
              const on = () => machines().filter(m => itemOf(m, c.name)?.on).length;
              return <th title={c.description}>{c.name}<small>{sourceWord(c)} · {on()} of {machines().length}</small></th>;
            }}</For>
          </tr>
        </thead>
        <tbody>
          <For each={machines()}>{m => (
            <tr class={{ away: away(m) }}>
              <td class="pm">
                <button class="pmname" tabindex={-1} title={`${m.name}'s Plugins page`} onClick={() => openPlugins(m)}>
                  <span class={['mdot', { up: reach[m.id] === 'up', down: away(m) }]} />{m.name}
                </button>
                <span class="pmnote">{emptyWhy(m) ?? (away(m) ? `Offline · as of ${ago(m.plugins!.at)}`
                  : m.plugins!.viewOnly ? 'View-only' : !m.plugins!.remoteInstall ? 'Installs only on the machine' : '')}</span>
              </td>
              <Show when={!emptyWhy(m)} fallback={<td class="pnone" colspan={columns().length}>—</td>}>
                <For each={columns()}>{c => {
                  const item = () => itemOf(m, c.name);
                  const can = () => offers(m, c);
                  const fetchOffer = () => can().find(o => o.t !== 'toggle');
                  return (
                    <td>
                      <Show when={item()} fallback={
                        <Show when={fetchOffer()} fallback={<span class="pnone">—</span>}>
                          {o => <FetchButton m={m} c={c} offer={o()} />}
                        </Show>
                      }>
                        {i => <>
                          <Switch on={i().on} stale={away(m)} busy={busy(m, c.name)}
                            disabled={!can().some(o => o.t === 'toggle')}
                            title={`${c.name} on ${m.name}: ${i().on ? 'on' : 'off'}`}
                            onToggle={() => void turn(m, c.name, !i().on)} />
                          <Show when={i().kind !== OWN && i().version}>
                            <span class={['pver', { old: !!fetchOffer() }]}>{i().version}</span>
                          </Show>
                          <Show when={fetchOffer()}>{o => <FetchButton m={m} c={c} offer={o()} />}</Show>
                        </>}
                      </Show>
                    </td>
                  );
                }}</For>
              </Show>
            </tr>
          )}</For>
        </tbody>
      </table>
    </div>
  );
}

// ---- one row per plugin ----

function Chip(props: { m: Machine; item?: PluginItem }) {
  const state = () => away(props.m) ? 'away' : !props.m.plugins ? 'unknown' : !props.item ? 'absent' : props.item.on ? 'on' : 'off';
  const words = () => ({ away: 'offline', unknown: 'not known', absent: 'not installed', off: 'off', on: '' })[state()];
  return (
    <button class={['pchip', { [state()]: true }]} tabindex={-1} title={`${props.m.name}'s Plugins page`}
      onClick={() => openPlugins(props.m)}>
      <Show when={state() === 'on'}><span class="mdot up" /></Show>
      {props.m.name}
      <Show when={words()}>{w => <span> · {w()}</span>}</Show>
      <Show when={props.item?.kind !== OWN && props.item?.version}>{v => <span> · {v()}</span>}</Show>
    </button>
  );
}

function PluginRow(props: { c: Column }) {
  const c = props.c;
  const known = () => machines().filter(m => m.plugins);
  const on = () => known().filter(m => itemOf(m, c.name)?.on);
  const everywhere = () => on().length === machines().length;
  /** Machines where it is off and could be turned on. */
  const off = () => known().filter(m => { const i = itemOf(m, c.name); return i && !i.on && offers(m, c).some(o => o.t === 'toggle'); });
  const fetches = () => known().flatMap(m => offers(m, c).filter(o => o.t !== 'toggle').map(o => ({ m, o })));
  return (
    <div class="panel prow">
      <div class="ptop">
        <span class="pic">{c.name[0]?.toUpperCase()}</span>
        <div class="pwhat">
          <b>{c.name}</b><span class="psrc">{sourceWord(c)}{c.latest ? ` · ${c.latest.version}` : ''}</span>
          <Show when={c.description}><p>{c.description}</p></Show>
        </div>
        <span class="pcount">{everywhere() ? 'On everywhere' : `On ${on().length} of ${machines().length}`}</span>
      </div>
      <Show when={!everywhere()}>
        <div class="pchips">
          <For each={machines()}>{m => <Chip m={m} item={itemOf(m, c.name)} />}</For>
        </div>
      </Show>
      <Show when={off().length || fetches().length}>
        <div class="pacts">
          <For each={off()}>{m => (
            <button class="btn small" tabindex={-1} disabled={busy(m, c.name)} onClick={() => void turn(m, c.name, true)}>
              {busy(m, c.name) ? 'Working…' : `Turn on ${m.name}`}
            </button>
          )}</For>
          <For each={fetches()}>{f => <FetchButton m={f.m} c={c} offer={f.o} long />}</For>
        </div>
      </Show>
    </div>
  );
}

function ByPlugin() {
  return <div class="prows"><For each={columns()}>{c => <PluginRow c={c} />}</For></div>;
}

export function PluginsOverview() {
  onSettled(() => { setPluginError(''); refreshAllPlugins(true); });
  const unknown = createMemo(() => machines().filter(m => emptyWhy(m) && reach[m.id] !== 'down'));
  return (
    <section class="plugins">
      <div class="phead">
        <p class="hint">Plugins on every machine. A machine's name opens its own Plugins page, for settings and
          for installing something new.</p>
        <div class="dtabs pseg" role="tablist">
          <button class="tb" tabindex={-1} aria-pressed={layout() === 'plugin' ? 'true' : 'false'} onClick={() => setLayout('plugin')}>By plugin</button>
          <button class="tb" tabindex={-1} aria-pressed={layout() === 'grid' ? 'true' : 'false'} onClick={() => setLayout('grid')}>Grid</button>
        </div>
      </div>
      <Show when={pluginError()}><p class="err banner">{pluginError()}</p></Show>
      <Show when={columns().length} fallback={<p class="hint">{unknown().length ? 'Asking the machines…' : 'No machine has said what it has yet.'}</p>}>
        <Show when={layout() === 'grid'} fallback={<ByPlugin />}><Grid /></Show>
      </Show>
      <Show when={layout() === 'plugin'}>
        <For each={machines().filter(m => emptyWhy(m))}>{m => (
          <p class="hint small pmiss">{m.name}: {emptyWhy(m)}</p>
        )}</For>
      </Show>
    </section>
  );
}
