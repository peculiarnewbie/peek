// Hosted site: the page of paired machines at `/`. Choosing one opens it
// at `/screen/<name>` (HostedScreen.tsx); the toolbar's back button (or the
// browser's) returns here. Beside it, `/plugins` has every machine's plugins
// (Plugins.tsx). Loaded only in the browser: the server renders Intro.tsx.
import { createSignal, For, onSettled, Show } from 'solid-js';
import {
  BLOCKED_HELP, cleanHost, favorite, forget, localAccess, machines, navigate, openMachine, pagePath, pair,
  parseLink, PLUGINS_PATH, probeAll, reach, screenPath, setFavorite, type Machine,
} from '../hosted';
import { changing, columns, fetchTo, offers, picked, setPicked, turn } from '../plugins';
import { cardError, cardItems, press, pressing } from '../views';
import { Icon } from './Icons';
import { AddForm, Foot, Header, Intro } from './Intro';
import { canScan, QrScan } from './QrScan';
import { PluginsOverview } from './Plugins';
import { ItemView } from './View';

const [error, setError] = createSignal('');
const [hostField, setHostField] = createSignal('');
const [codeField, setCodeField] = createSignal('');
const [busy, setBusy] = createSignal(false);
const [adding, setAdding] = createSignal(false);
const [scanning, setScanning] = createSignal(false);

async function submit(host: string, code: string) {
  setBusy(true);
  setError('');
  try {
    const m = await pair(host, code);
    setHostField('');
    setCodeField('');
    setAdding(false);
    openMachine(m);
  } catch (e) {
    setError((e as Error).message);
    setAdding(true);
  } finally {
    setBusy(false);
  }
}

/** Opened through a pairing link: pair straight away. */
export function pairFromUrl() {
  const link = parseLink(location.href);
  navigate('/', true);   // the code is spent either way; keep it out of history
  if (!link) return;
  setHostField(link.host);
  setCodeField(link.code);
  submit(link.host, link.code);
}

const STATUS = { up: 'Online', down: 'Offline', checking: 'Checking…' } as const;

function Card(props: { m: Machine }) {
  const state = () => reach[props.m.id] ?? 'checking';
  return (
    <div class={['dcard', { down: state() === 'down' }]}>
      <a class="dopen" href={screenPath(props.m)} onClick={e => {
        if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;   // new tab still works
        e.preventDefault();
        openMachine(props.m);
      }}>
        <span class="dicon"><Icon name="monitor" /></span>
        <span class="dname">{props.m.name}</span>
        <span class="dhost">{props.m.host}</span>
        <span class="dstate"><span class={['mdot', { up: state() === 'up', down: state() === 'down' }]} />
          {STATUS[state()]}</span>
      </a>
      <CardViews m={props.m} />
      <CardPlugins m={props.m} />
      <button class={['dfav tb icon', { on: favorite() === props.m.id }]} tabindex={-1}
        title={favorite() === props.m.id ? 'Favorite: the app icon’s shortcut opens it (tap to unset)'
          : 'Make favorite: long-press the app icon to open it'}
        onClick={() => setFavorite(favorite() === props.m.id ? null : props.m.id)}>
        <Icon name="star" />
      </button>
      <button class="dforget tb icon" tabindex={-1} title={`Forget ${props.m.name}`}
        onClick={() => { if (confirm(`Forget ${props.m.name}? Pairing it again needs a new code.`)) forget(props.m.id); }}>
        <Icon name="x" />
      </button>
    </div>
  );
}

/** What the machines' providers ask to be shown on this card (views.ts). */
function CardViews(props: { m: Machine }) {
  const shown = () => cardItems(props.m);
  return (
    <Show when={shown().length || cardError[props.m.id]}>
      <div class="dviews">
        <For each={shown()} keyed={false}>{s => (
          <ItemView item={s().item} disabled={pressing[props.m.id]} onPress={() => press(props.m, s())} />
        )}</For>
        <Show when={cardError[props.m.id]}>{why => <p class="vtext wrong">{why()}</p>}</Show>
      </div>
    </Show>
  );
}

/** The machine's plugins as tags, solid when on and dashed when off. With a
 *  plugin picked above the list, its tag stands out, and a card where it is
 *  off or missing offers to turn it on or install it. */
function CardPlugins(props: { m: Machine }) {
  const items = () => props.m.plugins?.items ?? [];
  const pick = () => { const name = picked(); return name ? columns().find(c => c.name === name) : undefined; };
  const pickedItem = () => items().find(i => i.name === picked());
  const fix = () => {
    const c = pick();
    if (!c || pickedItem()?.on || !props.m.plugins) return null;
    const can = offers(props.m, c);
    const fetch = can.find(o => o.t !== 'toggle');
    return { c, toggle: !!pickedItem() && can.some(o => o.t === 'toggle'), fetch: !pickedItem() && fetch?.t === 'install' ? fetch : undefined };
  };
  const working = () => !!changing[`${props.m.id}/${picked()}`];
  return (
    <Show when={items().length}>
      <div class="dplugins">
        <For each={items()}>{i => (
          <button class={['ptag', { off: !i.on, hl: i.name === picked() }]} tabindex={-1}
            title={`${i.name}: ${i.on ? 'on' : 'off'}. Opens ${props.m.name}'s Plugins page`}
            onClick={() => navigate(pagePath(props.m, 'plugins'))}>{i.name}</button>
        )}</For>
      </div>
      <Show when={fix()}>{f => (
        <div class="dfix">
          <span>{f().c.name} is {pickedItem() ? 'off' : 'not installed'}</span>
          <Show when={f().toggle}>
            <button class="btn small alt" tabindex={-1} disabled={working()}
              onClick={() => void turn(props.m, f().c.name, true)}>{working() ? 'Working…' : 'Turn on'}</button>
          </Show>
          <Show when={f().fetch}>{o => (
            <button class="btn small alt" tabindex={-1} disabled={working()} title={`From ${o().source}`}
              onClick={() => void fetchTo(props.m, f().c.name, o().source, false)}>{working() ? 'Working…' : 'Install'}</button>
          )}</Show>
        </div>
      )}</Show>
    </Show>
  );
}

/** Pick a plugin to compare the machines' cards by. */
function PluginFilter() {
  const known = () => machines().filter(m => m.plugins).length;
  return (
    <Show when={columns().length}>
      <div class="pfilter">
        <span>Plugins</span>
        <button class={['pill', { sel: !picked() }]} tabindex={-1} onClick={() => setPicked(null)}>All</button>
        <For each={columns()}>{c => (
          <button class={['pill', { sel: picked() === c.name }]} tabindex={-1}
            onClick={() => setPicked(picked() === c.name ? null : c.name)}>
            {c.name} <em>{machines().filter(m => m.plugins?.items.some(i => i.name === c.name && i.on)).length}/{known()}</em>
          </button>
        )}</For>
      </div>
    </Show>
  );
}

/** A whole pairing link pasted in the host field fills in both fields. */
function onHostInput(value: string) {
  const link = parseLink(value);
  if (link) { setHostField(link.host); setCodeField(link.code); } else setHostField(value);
}

/** The list of machines, or every machine's plugins (`/plugins`). */
export function MachinesPage(props: { plugins?: boolean }) {
  // Keep the status current while the page shows.
  onSettled(() => {
    probeAll();
    const timer = setInterval(probeAll, 10000);
    return () => clearInterval(timer);
  });
  const showForm = () => adding() || machines().length === 0;
  const onPlugins = () => !!props.plugins && machines().length > 0;
  return (
    <main id="devices">
      <Header>
        <Show when={machines().length && !showForm()}>
          <button class="tb" tabindex={-1} title="Add a machine" onClick={() => { setError(''); setAdding(true); navigate('/'); }}>
            <Icon name="plus" /><span>Add a machine</span>
          </button>
        </Show>
      </Header>
      <Show when={localAccess() === 'denied'}>
        <p class="err banner">{BLOCKED_HELP}</p>
      </Show>
      <Show when={machines().length}>
        <div class="dtabs-row">
          <div class="dtabs" role="tablist">
            <button class="tb" tabindex={-1} aria-pressed={!onPlugins() ? 'true' : 'false'} onClick={() => navigate('/')}>Machines</button>
            <button class="tb" tabindex={-1} aria-pressed={onPlugins() ? 'true' : 'false'} onClick={() => navigate(PLUGINS_PATH)}>Plugins</button>
          </div>
        </div>
      </Show>
      <Show when={!onPlugins()} fallback={<PluginsOverview />}>
        <Show when={machines().length} fallback={<Intro />}>
          <PluginFilter />
          <div class="dgrid">
            <For each={machines()}>{m => <Card m={m} />}</For>
          </div>
        </Show>
        <Show when={showForm()}>
          <AddForm host={hostField()} code={codeField()} busy={busy()} error={error()}
            ready={!busy() && !!cleanHost(hostField()) && !!codeField().trim()}
            scan={canScan()} cancel={machines().length > 0}
            onHost={onHostInput} onCode={setCodeField}
            onSubmit={() => { if (!busy()) submit(hostField(), codeField()); }}
            onScan={() => { setError(''); setScanning(true); }}
            onCancel={() => { setAdding(false); setError(''); }} />
        </Show>
        <Show when={scanning()}>
          <QrScan onClose={() => setScanning(false)} onLink={(host, code) => {
            setScanning(false);
            setHostField(host);
            setCodeField(code);
            submit(host, code);
          }} />
        </Show>
      </Show>
      <Foot />
    </main>
  );
}
