// Hosted client: the paired machines, switching between them, and pairing
// another one.
import { createEffect, createSignal, For, onCleanup, Show } from 'solid-js';
import { connect, disconnect } from '../connection';
import {
  cleanHost, current, forget, machines, machinesOpen, pair, parseLink, probeAll, reach, setCurrent,
  setMachinesOpen, type Machine,
} from '../hosted';
import { focusScreen } from '../screen';
import { setMessage } from '../state';
import { Icon } from './Icons';

const [error, setError] = createSignal('');
const [hostField, setHostField] = createSignal('');
const [codeField, setCodeField] = createSignal('');
const [busy, setBusy] = createSignal(false);

export function switchTo(m: Machine) {
  setMachinesOpen(false);
  if (current()?.id === m.id) { focusScreen(); return; }
  disconnect();
  setCurrent(m.id);
  setMessage(`Connecting to ${m.name}…`);
  connect();
  focusScreen();
}

async function submit(host: string, code: string) {
  setBusy(true);
  setError('');
  try {
    const m = await pair(host, code);
    setHostField('');
    setCodeField('');
    switchTo(m);
  } catch (e) {
    setError((e as Error).message);
    setMachinesOpen(true);
  } finally {
    setBusy(false);
  }
}

/** Opened through a pairing link: pair straight away. */
export function pairFromUrl() {
  const link = parseLink(location.href);
  history.replaceState(null, '', '/');   // the code is spent either way; keep it out of history
  if (!link) { connect(); return; }
  setHostField(link.host);
  setCodeField(link.code);
  setMessage(`Pairing with ${link.name ?? link.host}…`);
  submit(link.host, link.code).then(() => { if (!current()) setMessage(''); });
}

function Dot(props: { id: string }) {
  return <span class="mdot" classList={{ up: reach[props.id] === 'up', down: reach[props.id] === 'down' }} />;
}

export function Machines() {
  // Keep the dots current while the list is open.
  createEffect(() => {
    if (!machinesOpen()) return;
    probeAll();
    const timer = setInterval(probeAll, 10000);
    onCleanup(() => clearInterval(timer));
  });
  const onHostInput = (value: string) => {
    // A whole pairing link pasted here fills in both fields.
    const link = parseLink(value);
    if (link) { setHostField(link.host); setCodeField(link.code); } else setHostField(value);
  };
  const close = () => { setMachinesOpen(false); focusScreen(); };
  return (
    <div id="machines" class="panel" classList={{ show: machinesOpen() }} role="dialog" aria-label="Machines">
      <div class="mhead">
        <span class="t">Machines</span>
        <Show when={current()}>
          <button class="tb icon" tabIndex={-1} title="Close" onClick={close}><Icon name="x" /></button>
        </Show>
      </div>
      <Show when={machines().length} fallback={
        <p class="hint">peek connects this browser straight to your computers over Tailscale. Pair it with
          each one once.</p>
      }>
        <div class="mlist">
          <For each={machines()}>{m =>
            <div class="mrow" classList={{ cur: current()?.id === m.id }}>
              <button class="mpick" tabIndex={-1} onClick={() => switchTo(m)}>
                <Dot id={m.id} />
                <span class="mname">{m.name}</span>
                <span class="mhost">{m.host}</span>
              </button>
              <button class="tb icon" tabIndex={-1} title={`Forget ${m.name}`}
                onClick={() => { if (confirm(`Forget ${m.name}? Pairing it again needs a new code.`)) forget(m.id); }}>
                <Icon name="x" />
              </button>
            </div>
          }</For>
        </div>
      </Show>
      <div class="madd">
        <div class="label"><span>Add a machine</span></div>
        <p class="hint">On the machine, run <code>peek pair</code>. Open the link it shows on this device (or scan
          its QR code), or enter its name and code here.</p>
        <form onSubmit={e => { e.preventDefault(); if (!busy()) submit(hostField(), codeField()); }}>
          <input class="field" placeholder="my-pc.tail1234.ts.net or pairing link" autocomplete="off"
            autocapitalize="off" spellcheck={false} value={hostField()} onInput={e => onHostInput(e.currentTarget.value)} />
          <div class="mform-row">
            <input class="field" placeholder="Code" autocomplete="one-time-code" autocapitalize="characters"
              spellcheck={false} value={codeField()} onInput={e => setCodeField(e.currentTarget.value)} />
            <button class="btn" type="submit" disabled={busy() || !cleanHost(hostField()) || !codeField().trim()}>
              {busy() ? 'Pairing…' : 'Pair'}
            </button>
          </div>
        </form>
        <Show when={error()}><p class="err">{error()}</p></Show>
        <p class="hint small">This browser reaches each machine over Tailscale. Nothing about your machines is
          stored on this site.</p>
      </div>
    </div>
  );
}
