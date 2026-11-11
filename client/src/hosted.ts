// The hosted client: this page served from its own site (not by a host),
// paired with any number of machines and connecting to whichever is chosen.
// Each machine is reached straight from this browser at its Tailscale HTTPS
// name (Tailscale Serve, which `peek pair` sets up), with a token of this browser's
// own. Nothing about the machines leaves this browser.
import { createSignal } from 'solid-js';
import { createStore } from 'solid-js/store';
import { store } from './util';

export const HOSTED = import.meta.env.MODE === 'hosted';

export interface Machine {
  /** The host's own id, so a machine keeps its place whatever it is reached at. */
  id: string;
  name: string;
  /** Its Tailscale HTTPS name, e.g. hp-book.tail1234.ts.net. */
  host: string;
  token: string;
  added: number;
}

function load(): Machine[] {
  try {
    const list = JSON.parse(store.get('machines') ?? '[]');
    return Array.isArray(list) ? list.filter(m => m && m.id && m.host && m.token) : [];
  } catch { return []; }
}
const [machines, setMachinesRaw] = createSignal<Machine[]>(load());
export { machines };
function setMachines(list: Machine[]) {
  setMachinesRaw(list);
  store.set('machines', JSON.stringify(list));
}

const [currentId, setCurrentIdRaw] = createSignal<string | null>(store.get('current'));
export const current = () => machines().find(m => m.id === currentId()) ?? null;
export function setCurrent(id: string | null) {
  setCurrentIdRaw(id);
  store.set('current', id ?? '');
}

/** The machine list: open on its own when there is nothing to show yet. */
export const [machinesOpen, setMachinesOpen] = createSignal(false);

export function forget(id: string) {
  setMachines(machines().filter(m => m.id !== id));
  if (currentId() === id) setCurrent(null);
}

// ---- reachability ----
export type Reach = 'checking' | 'up' | 'down';
export const [reach, setReach] = createStore<Record<string, Reach>>({});

interface About { id: string; name: string; protocol: number }

async function about(host: string, ms = 5000): Promise<About> {
  const r = await fetch(`https://${host}/.well-known/peek`, { cache: 'no-store', signal: AbortSignal.timeout(ms) });
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  return r.json();
}

/** Whether each machine answers, for the dots in the list. */
export function probeAll() {
  for (const m of machines()) {
    if (!reach[m.id]) setReach(m.id, 'checking');
    about(m.host, 4000)
      .then(a => setReach(m.id, a.id === m.id ? 'up' : 'down'))
      .catch(() => setReach(m.id, 'down'));
  }
}

// ---- pairing ----
/** A host and code from a pairing link (`peek pair` prints one), if it is one. */
export function parseLink(text: string): { host: string; code: string; name?: string } | null {
  try {
    const url = new URL(text.trim());
    const host = url.searchParams.get('host');
    const code = new URLSearchParams(url.hash.slice(1)).get('code');
    if (host && code) return { host, code, name: url.searchParams.get('name') ?? undefined };
  } catch {}
  return null;
}

/** A machine as typed: no scheme, no path, no trailing dot. */
export const cleanHost = (text: string) =>
  text.trim().replace(/^https?:\/\//i, '').replace(/\/.*$/, '').replace(/\.$/, '').toLowerCase();

/** How this browser is listed on the machine (`peek clients`). */
function deviceLabel() {
  const ua = navigator.userAgent;
  const os = /Android/.test(ua) ? 'Android' : /iPhone|iPad/.test(ua) ? 'iOS' : /Windows/.test(ua) ? 'Windows'
    : /Mac OS/.test(ua) ? 'macOS' : /Linux/.test(ua) ? 'Linux' : 'unknown';
  const browser = /Edg\//.test(ua) ? 'Edge' : /Firefox\//.test(ua) ? 'Firefox' : /Chrome\//.test(ua) ? 'Chrome'
    : /Safari\//.test(ua) ? 'Safari' : 'browser';
  return `${browser} on ${os}`;
}

/** Trade a one-time code for this browser's own token, and add the machine. */
export async function pair(hostText: string, code: string): Promise<Machine> {
  const host = cleanHost(hostText);
  if (!host) throw new Error('Enter the machine’s Tailscale name, like my-pc.tail1234.ts.net.');
  let info: About;
  try {
    // Long enough for Tailscale to fetch the machine's certificate on its first HTTPS request.
    info = await about(host, 20000);
  } catch {
    throw new Error(`Cannot reach ${host}. Check that Tailscale is connected on this device, and that the ` +
      'machine runs peek and `peek pair` worked there. If the browser asked to access devices on ' +
      'your local network, allow it.');
  }
  let r: Response;
  try {
    r = await fetch(`https://${host}/api/pair`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ code, label: deviceLabel() }),
      cache: 'no-store',
    });
  } catch {
    throw new Error(`${info.name} did not answer. Try again.`);
  }
  if (r.status === 403) {
    const why = await r.json().catch(() => ({}));
    throw new Error(why.error === 'origin'
      ? `${info.name} does not accept this site. Start peek there with --web ${location.origin}.`
      : 'That code is wrong, used or expired. Run `peek pair` on the machine for a new one.');
  }
  if (r.status === 429) throw new Error('Too many wrong codes. Wait a minute and try again.');
  if (!r.ok) throw new Error(`${info.name} refused to pair (HTTP ${r.status}).`);
  const res = await r.json();
  const machine: Machine = { id: res.id, name: res.name || info.name, host, token: res.token, added: Date.now() };
  setMachines([...machines().filter(m => m.id !== machine.id), machine]);
  setReach(machine.id, 'up');
  return machine;
}
