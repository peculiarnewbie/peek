// Hosted client: the page of paired machines at `/`. Choosing one opens it
// at `/screen/<name>`; the toolbar's back button (or the browser's) returns here.
// Beside it, `/plugins` has every machine's plugins (Plugins.tsx).
import { createEffect, createMemo, createSignal, For, Index, on, onCleanup, Show, untrack } from 'solid-js';
import { showSettings } from '../actions';
import { connect, disconnect } from '../connection';
import {
  about, BLOCKED_HELP, cleanHost, current, favorite, forget, learn, localAccess, machines, navigate, onPluginsPage,
  openMachine, pagePath, pair, parseLink, PLUGINS_PATH, probeAll, reach, screenPath, setFavorite, type Machine,
} from '../hosted';
import { changing, columns, fetchTo, offers, picked, setPicked, turn } from '../plugins';
import { focusScreen } from '../screen';
import { setMessage } from '../state';
import { cardError, cardItems, press, pressing } from '../views';
import { Icon } from './Icons';
import { canScan, QrScan } from './QrScan';
import { PluginsOverview } from './Plugins';
import { ItemView } from './View';

/** Shell commands, one per line, with one button that copies them all. */
function Commands(props: { lines: string[] }) {
  const [copied, setCopied] = createSignal(false);
  let timer: number | undefined;
  const copy = () => {
    navigator.clipboard.writeText(props.lines.join('\n')).then(() => {
      setCopied(true);
      clearTimeout(timer);
      timer = window.setTimeout(() => setCopied(false), 1200);
    }, () => {});
  };
  return (
    <div class="cmds">
      <code>{props.lines.join('\n')}</code>
      <button type="button" class="cmd-copy" onClick={copy} aria-label="Copy commands" title="Copy">
        <Icon name={copied() ? 'check' : 'copy'} />
      </button>
    </div>
  );
}

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

const GITHUB = 'https://github.com/peculiarnewbie/peek';

const STATUS = { up: 'Online', down: 'Offline', checking: 'Checking…' } as const;

function Card(props: { m: Machine }) {
  const state = () => reach[props.m.id] ?? 'checking';
  return (
    <div class="dcard" classList={{ down: state() === 'down' }}>
      <a class="dopen" href={screenPath(props.m)} onClick={e => {
        if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;   // new tab still works
        e.preventDefault();
        openMachine(props.m);
      }}>
        <span class="dicon"><Icon name="monitor" /></span>
        <span class="dname">{props.m.name}</span>
        <span class="dhost">{props.m.host}</span>
        <span class="dstate"><span class="mdot" classList={{ up: state() === 'up', down: state() === 'down' }} />
          {STATUS[state()]}</span>
      </a>
      <CardViews m={props.m} />
      <CardPlugins m={props.m} />
      <button class="dfav tb icon" classList={{ on: favorite() === props.m.id }} tabIndex={-1}
        title={favorite() === props.m.id ? 'Favorite: the app icon’s shortcut opens it (tap to unset)'
          : 'Make favorite: long-press the app icon to open it'}
        onClick={() => setFavorite(favorite() === props.m.id ? null : props.m.id)}>
        <Icon name="star" />
      </button>
      <button class="dforget tb icon" tabIndex={-1} title={`Forget ${props.m.name}`}
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
        <Index each={shown()}>{s => (
          <ItemView item={s().item} disabled={pressing[props.m.id]} onPress={() => press(props.m, s())} />
        )}</Index>
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
          <button class="ptag" classList={{ off: !i.on, hl: i.name === picked() }} tabIndex={-1}
            title={`${i.name}: ${i.on ? 'on' : 'off'}. Opens ${props.m.name}'s Plugins page`}
            onClick={() => navigate(pagePath(props.m, 'plugins'))}>{i.name}</button>
        )}</For>
      </div>
      <Show when={fix()}>{f => (
        <div class="dfix">
          <span>{f().c.name} is {pickedItem() ? 'off' : 'not installed'}</span>
          <Show when={f().toggle}>
            <button class="btn small alt" tabIndex={-1} disabled={working()}
              onClick={() => void turn(props.m, f().c.name, true)}>{working() ? 'Working…' : 'Turn on'}</button>
          </Show>
          <Show when={f().fetch}>{o => (
            <button class="btn small alt" tabIndex={-1} disabled={working()} title={`From ${o().source}`}
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
        <button class="pill" classList={{ sel: !picked() }} tabIndex={-1} onClick={() => setPicked(null)}>All</button>
        <For each={columns()}>{c => (
          <button class="pill" classList={{ sel: picked() === c.name }} tabIndex={-1}
            onClick={() => setPicked(picked() === c.name ? null : c.name)}>
            {c.name} <em>{machines().filter(m => m.plugins?.items.some(i => i.name === c.name && i.on)).length}/{known()}</em>
          </button>
        )}</For>
      </div>
    </Show>
  );
}

export function Devices() {
  // The page decides the connection: a machine's page connects to it, the
  // list (or a machine no longer paired) drops it.
  // (A memo: the list of machines changes whenever one says something new
  // of itself, and that must not reconnect the one being shown.)
  const shownId = createMemo(() => current()?.id);
  createEffect(on(shownId, id => {
    disconnect();
    if (id) {
      setMessage(`Connecting to ${current()!.name}…`);
      connect();
      // Its providers as they are now: this page may be the first to ask.
      const m = current()!;
      about(m.host).then(a => { if (a.id === m.id) learn(m, a); }, () => {});
      focusScreen();
    } else {
      showSettings(false);
      setMessage('');
      if (location.pathname !== '/' && location.pathname !== '/pair') navigate('/', true);
    }
  }));
  // Keep the status current while the list shows.
  createEffect(() => {
    if (shownId()) return;
    // Untracked: probing reads the list, and what it learns changes the
    // list, which must not start it over.
    untrack(probeAll);
    const timer = setInterval(probeAll, 10000);
    onCleanup(() => clearInterval(timer));
  });
  createEffect(() => document.body.classList.toggle('home', !current()));

  const onHostInput = (value: string) => {
    // A whole pairing link pasted here fills in both fields.
    const link = parseLink(value);
    if (link) { setHostField(link.host); setCodeField(link.code); } else setHostField(value);
  };
  const showForm = () => adding() || machines().length === 0;
  return (
    <Show when={!current()}>
      <main id="devices">
        <header class="dhead">
          <h1><img src="/icon.svg" alt="" width="28" height="28" />peek</h1>
          <nav class="dnav">
            <Show when={machines().length && !showForm()}>
              <button class="tb" tabIndex={-1} title="Add a machine" onClick={() => { setError(''); setAdding(true); navigate('/'); }}>
                <Icon name="plus" /><span>Add a machine</span>
              </button>
            </Show>
            <a class="tb" href="/docs/">Docs</a>
            <a class="tb" href="/changelog/">Changelog</a>
            <a class="tb" href={GITHUB}>GitHub</a>
          </nav>
        </header>
        <Show when={localAccess() === 'denied'}>
          <p class="err banner">{BLOCKED_HELP}</p>
        </Show>
        <Show when={machines().length}>
          <div class="dtabs" role="tablist">
            <button class="tb" tabIndex={-1} aria-pressed={!onPluginsPage()} onClick={() => navigate('/')}>Machines</button>
            <button class="tb" tabIndex={-1} aria-pressed={onPluginsPage()} onClick={() => navigate(PLUGINS_PATH)}>Plugins</button>
          </div>
        </Show>
        <Show when={!(onPluginsPage() && machines().length)} fallback={<PluginsOverview />}>
        <Show when={machines().length} fallback={
          <section class="intro">
            <p class="hint lead">peek is a small, fast remote desktop, tailored for short sessions.</p>
            <p class="hint lead">It also lets a machine show its vitals, sleep and wake, share folders, and run{' '}
              <a href="/docs/#apps">plugins</a>: programs on the machine that show up in your browser through peek.</p>
            <p class="hint lead">Run it on a computer (Windows, or Linux on Wayland) on your{' '}
              <a href="https://tailscale.com">Tailscale</a> network and pair this browser with it once. Its screen
              is then a click away, with mouse, keyboard and clipboard.</p>
            <p class="hint lead">This site only serves the page: your browser connects straight to each machine
              over Tailscale.</p>
            <Commands lines={['npx @peculiarnewbie/peek pair']} />
            <dl class="figures">
              <div><dt>140 ms</dt><dd>to a whole full-screen frame over Tailscale, from idle</dd></div>
              <div><dt>22 ms</dt><dd>the same on the machine itself</dd></div>
              <div><dt>0.02%</dt><dd>of a CPU core while nobody looks</dd></div>
              <div><dt>18 MB</dt><dd>of memory, in a 4.6 MB program</dd></div>
            </dl>
            <a class="figures-how" href="/docs/#numbers">How these were measured</a>
          </section>
        }>
          <PluginFilter />
          <div class="dgrid">
            <For each={machines()}>{m => <Card m={m} />}</For>
          </div>
        </Show>
        <Show when={showForm()}>
          <section class="dadd panel">
            <div class="label"><span>Add a machine</span></div>
            <p class="hint">On the machine, run <code>peek pair</code> (or, without installing peek,{' '}
              <code>npx @peculiarnewbie/peek pair</code>). Open the link it shows on this device (or scan its QR
              code), or enter its name and code here.</p>
            <Show when={canScan()}>
              <button class="btn alt scan" tabIndex={-1} onClick={() => { setError(''); setScanning(true); }}>
                <Icon name="qr" /><span>Scan QR code</span>
              </button>
            </Show>
            <form onSubmit={e => { e.preventDefault(); if (!busy()) submit(hostField(), codeField()); }}>
              <input class="field" placeholder="my-pc.tail1234.ts.net or pairing link" autocomplete="off"
                autocapitalize="off" spellcheck={false} value={hostField()}
                onInput={e => onHostInput(e.currentTarget.value)} />
              <div class="mform-row">
                <input class="field" placeholder="Code" autocomplete="one-time-code" autocapitalize="characters"
                  spellcheck={false} value={codeField()} onInput={e => setCodeField(e.currentTarget.value)} />
                <button class="btn" type="submit" disabled={busy() || !cleanHost(hostField()) || !codeField().trim()}>
                  {busy() ? 'Pairing…' : 'Pair'}
                </button>
              </div>
            </form>
            <Show when={error()}><p class="err">{error()}</p></Show>
            <Show when={machines().length}>
              <button class="tb cancel" tabIndex={-1} onClick={() => { setAdding(false); setError(''); }}>Cancel</button>
            </Show>
          </section>
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
        <p class="hint small foot">The list of machines and their keys stay in this browser. Nothing about them is
          stored on this site.</p>
      </main>
    </Show>
  );
}
