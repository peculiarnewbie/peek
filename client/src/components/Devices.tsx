// Hosted client: the page of paired machines at `/`. Choosing one opens it
// at `/screen/<name>`; the toolbar's back button (or the browser's) returns here.
import { createEffect, createSignal, For, on, onCleanup, Show } from 'solid-js';
import { showSettings } from '../actions';
import { connect, disconnect } from '../connection';
import {
  BLOCKED_HELP, cleanHost, current, forget, localAccess, machines, navigate, openMachine, pair, parseLink,
  probeAll, reach, screenPath, type Machine,
} from '../hosted';
import { doing, offer, sleep, TEST_WAKE_AFTER, wake, waker } from '../power';
import { focusScreen } from '../screen';
import { setMessage } from '../state';
import { Icon } from './Icons';
import { canScan, QrScan } from './QrScan';

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
      <Power m={props.m} up={state() === 'up'} down={state() === 'down'} />
      <button class="dforget tb icon" tabIndex={-1} title={`Forget ${props.m.name}`}
        onClick={() => { if (confirm(`Forget ${props.m.name}? Pairing it again needs a new code.`)) forget(props.m.id); }}>
        <Icon name="x" />
      </button>
    </div>
  );
}

/** Sleep and wake, under a machine's card: what is under way, or what it offers. */
function Power(props: { m: Machine; up: boolean; down: boolean }) {
  const what = () => offer(props.m);
  const testSleep = (by: Machine) => {
    const ifNot = props.m.power?.timer
      ? 'If that does not wake it, it wakes itself after about two minutes and says so.'
      : 'If that does not wake it, someone will have to press its power button: wake timers are off in its ' +
        'power settings, so it cannot wake itself.';
    if (confirm(`${props.m.name} will go to sleep, and ${by.name} will try to wake it about ${TEST_WAKE_AFTER} ` +
      `seconds later.\n\n${ifNot} Go ahead?`)) sleep(props.m, true);
  };
  const plainSleep = (by: Machine) => {
    if (confirm(`Put ${props.m.name} to sleep? ${by.name} can wake it from here.`)) sleep(props.m, false);
  };
  return (
    <Show when={doing[props.m.id]} fallback={
      <>
        <Show when={props.up && what()}>{o => {
          const x = o();
          return x.kind === 'none'
            ? <p class="dpower hint small">{`Sleep is off: ${x.why}`}</p>
            : <>
                <Show when={x.kind === 'test' && x.failed}>{why => <p class="dpower small err">{why()}</p>}</Show>
                <div class="dpower">
                  <button class="btn alt small" tabIndex={-1}
                    onClick={() => x.kind === 'test' ? testSleep(x.by) : plainSleep(x.by)}>
                    {x.kind === 'test' ? 'Test sleep and wake' : 'Sleep'}
                  </button>
                </div>
              </>;
        }}</Show>
        <Show when={props.down && props.m.power?.wake && waker(props.m)}>{by => (
          <div class="dpower">
            <button class="btn small" tabIndex={-1} onClick={() => wake(props.m)}>Wake</button>
            <span class="hint small">{`from ${by().name}`}</span>
          </div>
        )}</Show>
      </>
    }>{d => <p class="dpower small" classList={{ err: !!d().error, hint: !d().error, busy: d().busy }}>{d().text}</p>}</Show>
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
            <a class="tb" href="/changelog/">Changelog</a>
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
            <Commands lines={['npx @peculiarnewbie/peek pair']} />
            <ul class="figures">
              <li><b>125 ms</b>from connecting to a whole 1080p frame over Tailscale, with the machine idle until then</li>
              <li><b>24 ms</b>the same on the machine itself, peek's own share</li>
              <li><b>0.01%</b>of one CPU core while nobody is looking: no capture runs</li>
              <li><b>17 MB</b>of memory, in a 3.6 MB program</li>
              <li><a href="/docs/#numbers">How these were measured</a></li>
            </ul>
          </section>
        }>
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
        <p class="hint small foot">The list of machines and their keys stay in this browser. Nothing about them is
          stored on this site.</p>
      </main>
    </Show>
  );
}
