// The hosted client: this page served from its own site (not by a host),
// paired with any number of machines and connecting to whichever is chosen.
// Each machine is reached straight from this browser at its Tailscale HTTPS
// name (Tailscale Serve, which `peek pair` sets up), with a token of this browser's
// own. Nothing about the machines leaves this browser.
import { createSignal } from 'solid-js';
import { createStore } from 'solid-js/store';
import { dropCards, onAgain, refreshCards, type ProviderAbout } from './views';
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
  /** Its providers, as it last said (see views.ts). */
  providers?: ProviderAbout[];
  /** What each of its providers lets the same provider on the other
   *  machines know of it, kept for while it is away. */
  records?: Record<string, unknown>;
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

// ---- pages ----
// `/` lists the machines; `/screen/<name>` shows one (its Tailscale name's
// first part, unique in a tailnet). Plain history entries, so Back returns to
// the list. The site's own pages (/docs/, /changelog/, /pair) sit beside
// /screen/, so no machine's name can take one of their addresses.
const SCREEN = '/screen/';
const slug = (m: Machine) => m.host.split('.')[0].toLowerCase();
export const screenPath = (m: Machine) => SCREEN + slug(m);
// Until 0.2.0 a machine's page was `/<name>` (or `/<id>`); bookmarks of one
// still lead to it.
if (HOSTED && /^\/[^/]+$/.test(location.pathname) && location.pathname !== '/pair') {
  history.replaceState(null, '', SCREEN + location.pathname.slice(1) + location.search + location.hash);
}
const [path, setPath] = createSignal(location.pathname);
addEventListener('popstate', () => setPath(location.pathname));
export function navigate(to: string, replace = false) {
  if (to === location.pathname) return;
  history[replace ? 'replaceState' : 'pushState'](null, '', to);
  setPath(to);
}
/** The machine this page shows, or null on the list. */
export const current = () => {
  if (!path().startsWith(SCREEN)) return null;
  let name: string;
  try { name = decodeURIComponent(path().slice(SCREEN.length)).replace(/\/+$/, '').toLowerCase(); } catch { return null; }
  return (name && machines().find(m => slug(m) === name || m.id === name)) || null;
};
export const openMachine = (m: Machine) => navigate(screenPath(m));
export const home = () => navigate('/');

export function forget(id: string) {
  setMachines(machines().filter(m => m.id !== id));
}

export function updateMachine(id: string, patch: Partial<Machine>) {
  setMachines(machines().map(m => m.id === id ? { ...m, ...patch } : m));
}

// ---- Chrome's Local Network Access ----
// Chrome asks before a public site may reach addresses like Tailscale's
// 100.x.y.z; once blocked, every request fails without saying why.
export type LocalAccess = 'granted' | 'prompt' | 'denied' | 'unknown';
export const [localAccess, setLocalAccess] = createSignal<LocalAccess>('unknown');
(async () => {
  try {
    const status = await navigator.permissions.query({ name: 'local-network-access' as PermissionName });
    const update = () => setLocalAccess(status.state as LocalAccess);
    update();
    status.addEventListener('change', update);
  } catch {}   // not Chrome, or a Chrome without the prompt: nothing to ask for
})();
export const BLOCKED_HELP = 'This browser is blocking the site from reaching devices on your network. Click the ' +
  'icon left of the address, allow "Local network access", and reload.';

// ---- reachability ----
export type Reach = 'checking' | 'up' | 'down';
export const [reach, setReach] = createStore<Record<string, Reach>>({});

interface About { id: string; name: string; protocol: number; providers?: ProviderAbout[] }

/** A machine's providers as it says them now, kept if they changed. */
export function learn(m: Machine, a: About) {
  const providers = (Array.isArray(a.providers) ? a.providers : []).filter(p => p && typeof p.name === 'string');
  if (JSON.stringify(providers) !== JSON.stringify(m.providers ?? [])) updateMachine(m.id, { providers });
}

export async function about(host: string, ms = 5000): Promise<About> {
  const r = await fetch(`https://${host}/.well-known/peek`, { cache: 'no-store', signal: AbortSignal.timeout(ms) });
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  return r.json();
}

/** Whether each machine answers, for the dots in the list, and then what
 *  the ones that do have to show on the cards. */
export function probeAll() {
  const probes = machines().map(m => {
    if (!reach[m.id]) setReach(m.id, 'checking');
    return about(m.host, 4000)
      .then(a => {
        if (a.id !== m.id) throw new Error('another machine answers there');
        setReach(m.id, 'up');
        learn(m, a);
      })
      .catch(() => {
        setReach(m.id, 'down');
        dropCards(m.id);
      });
  });
  // Once it is known of every machine: a provider is told which of the
  // others are there, and "not yet known" would read as "away".
  Promise.all(probes).then(() => {
    for (const m of machines()) if (reach[m.id] === 'up') refreshCards(m);
  });
}
// A provider with something under way has every machine looked at again.
onAgain(probeAll);

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
    if (localAccess() === 'denied') throw new Error(BLOCKED_HELP);
    throw new Error(`Cannot reach ${host}. Check that Tailscale is connected on this device, and that the ` +
      'machine runs peek and `peek pair` worked there. If the browser asks to access devices on your ' +
      'local network, allow it.');
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
