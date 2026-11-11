// Hosted client: the page of paired machines at `/`. Choosing one opens it
// at `/<name>`; the toolbar's back button (or the browser's) returns here.
import { createEffect, createSignal, For, on, onCleanup, Show } from 'solid-js';
import { showSettings } from '../actions';
import { connect, disconnect } from '../connection';
import {
  BLOCKED_HELP, cleanHost, current, forget, localAccess, machines, navigate, openMachine, pair, parseLink,
  probeAll, reach, slug, type Machine,
} from '../hosted';
import { focusScreen } from '../screen';
import { setMessage } from '../state';
import { Icon } from './Icons';
import { canScan, QrScan } from './QrScan';

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
      <a class="dopen" href={`/${slug(props.m)}`} onClick={e => {
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
      <button class="dforget tb icon" tabIndex={-1} title={`Forget ${props.m.name}`}
        onClick={() => { if (confirm(`Forget ${props.m.name}? Pairing it again needs a new code.`)) forget(props.m.id); }}>
        <Icon name="x" />
      </button>
    </div>
  );
}

export function Devices() {
  // The page decides the connection: a machine's page connects to it, the
  // list (or a machine no longer paired) drops it.
  createEffect(on(() => current()?.id, id => {
    disconnect();
    if (id) {
      setMessage(`Connecting to ${current()!.name}…`);
      connect();
      focusScreen();
    } else {
      showSettings(false);
      setMessage('');
      if (location.pathname !== '/' && location.pathname !== '/pair') navigate('/', true);
    }
  }));
  // Keep the status current while the list shows.
  createEffect(() => {
    if (current()) return;
    probeAll();
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
              <button class="tb" tabIndex={-1} title="Add a machine" onClick={() => { setError(''); setAdding(true); }}>
                <Icon name="plus" /><span>Add a machine</span>
              </button>
            </Show>
            <a class="tb" href="/docs/">Docs</a>
            <a class="tb" href={GITHUB}>GitHub</a>
          </nav>
        </header>
        <Show when={localAccess() === 'denied'}>
          <p class="err banner">{BLOCKED_HELP}</p>
        </Show>
        <Show when={machines().length} fallback={
          <section class="intro">
            <p class="hint lead">peek is a small, fast remote desktop, tailored for short sessions.</p>
            <p class="hint lead">Run it on a computer (Windows, or Linux on Wayland) on your{' '}
              <a href="https://tailscale.com">Tailscale</a> network and pair this browser with it once. Its screen
              is then a click away, with mouse, keyboard and clipboard.</p>
            <p class="hint lead">This site only serves the page: your browser connects straight to each machine
              over Tailscale.</p>
            <div class="figures">
              <div><b>24<small> ms</small></b><span>from connecting to a whole 1080p frame, with the
                machine idle until then</span></div>
              <div><b>0.01<small> %</small></b><span>of one CPU core while nobody is looking: no capture runs</span></div>
              <div><b>17<small> MB</small></b><span>of memory, in a 3.6 MB program</span></div>
            </div>
            <p class="hint small">Measured on the machine itself (Linux, 1920×1080); your network adds its round
              trips. <a href="/docs/#numbers">How these were measured</a></p>
          </section>
        }>
          <div class="dgrid">
            <For each={machines()}>{m => <Card m={m} />}</For>
          </div>
        </Show>
        <Show when={showForm()}>
          <section class="dadd panel">
            <div class="label"><span>Add a machine</span></div>
            <p class="hint">On the machine, run <code>peek pair</code>. Open the link it shows on this device (or
              scan its QR code), or enter its name and code here.</p>
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
        <p class="hint small foot">The list of machines and their keys stay in this browser. Nothing about them is
          stored on this site.</p>
      </main>
    </Show>
  );
}
