// Every paired machine's plugins, side by side (hosted client). Each machine
// is asked what its Plugins page would list, at /api/plugins with this
// browser's token, and the answer is kept with the machine so one that is
// away still shows what it last said. Changes go to the same place, under
// the same rules as on the page: a view-only machine only answers `list`,
// and one with `peek plugin remote-install off` refuses installs and updates.
import { createSignal } from 'solid-js';
import { createStore } from 'solid-js';
import { about, learn, machines, reach, updateMachine, type Machine } from './hosted';
import { store } from './util';

export interface PluginItem {
  name: string;
  /** "peek's own", "a plugin" or "a command" (`peek provider add`). */
  kind: string;
  description: string;
  on: boolean;
  version?: string;
  /** Where a plugin was installed from: a folder there, an https git address, or npm. */
  from?: string | null;
  missing?: string[];
}

export interface PluginList {
  items: PluginItem[];
  remoteInstall: boolean;
  viewOnly: boolean;
  /** When the machine said it. */
  at: number;
}

export const OWN = "peek's own";
export const PLUGIN = 'a plugin';

/** How asking a machine for its plugins went, while it is not plain: under
 *  way, or a peek too old to answer. */
export const [pluginState, setPluginState] = createStore<Record<string, 'loading' | 'old' | undefined>>({});
/** A change under way, by `<machine id>/<plugin>`. */
export const [changing, setChanging] = createStore<Record<string, boolean | undefined>>({});
/** What went wrong with the last change, and where. */
export const [pluginError, setPluginError] = createSignal('');

/** The overview's layout: a grid of machines by plugins, or one row per plugin. */
export type Layout = 'grid' | 'plugin';
const [layout, setLayoutRaw] = createSignal<Layout>(store.get('plugins-layout') === 'grid' ? 'grid' : 'plugin');
export { layout };
export function setLayout(l: Layout) { setLayoutRaw(l); store.set('plugins-layout', l); }

/** The plugin picked on the list of machines, to compare their cards by. */
const [picked, setPickedRaw] = createSignal<string | null>(store.get('plugins-picked'));
export { picked };
export function setPicked(name: string | null) {
  setPickedRaw(name);
  if (name) store.set('plugins-picked', name); else localStorage.removeItem('peek-plugins-picked');
}

async function post(m: Machine, body: object, ms = 15000): Promise<Record<string, unknown>> {
  const r = await fetch(`https://${m.host}/api/plugins`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ ...body, token: m.token }),
    cache: 'no-store',
    signal: AbortSignal.timeout(ms),
  });
  const answer = await r.json().catch(() => ({ t: 'error', why: `HTTP ${r.status}` }));
  if (answer.t === 'error') throw new Error(answer.why ?? 'it refused');
  return answer;
}

/** Ask a machine for its plugins: if what it last said is a minute old, or
 *  `force`. */
export async function refreshPlugins(m: Machine, force = false) {
  // (A plain set: the store's 'loading' is read only after the update lands,
  // and probeAll can ask twice in a row.)
  if (asking.has(m.id)) return;
  if (!force && m.plugins && Date.now() - m.plugins.at < 60_000) return;
  asking.add(m.id);
  setPluginState(s => { s[m.id] = 'loading'; });
  try {
    const a = await post(m, { t: 'list' }, 8000);
    const items = (Array.isArray(a.items) ? a.items : [])
      .filter((i): i is PluginItem => !!i && typeof i.name === 'string' && i.name !== 'screen')
      .map(i => ({
        name: i.name, kind: String(i.kind ?? ''), description: String(i.description ?? ''), on: !!i.on,
        version: typeof i.version === 'string' && i.version ? i.version : undefined,
        from: typeof i.from === 'string' ? i.from : null,
        missing: Array.isArray(i.missing) ? i.missing.map(String) : [],
      }));
    const fresh = machines().find(o => o.id === m.id);
    if (fresh) updateMachine(m.id, { plugins: { items, remoteInstall: !!a.remote_install, viewOnly: !!a.view_only, at: Date.now() } });
    setPluginState(s => { delete s[m.id]; });
  } catch (e) {
    // A machine that answers its address but not this is a peek from
    // before 0.3.1. (Without CORS headers, a 404 reads as a network error.)
    const old = reach[m.id] === 'up' && e instanceof TypeError;
    setPluginState(s => { if (old) s[m.id] = 'old'; else delete s[m.id]; });
  } finally {
    asking.delete(m.id);
  }
}
const asking = new Set<string>();

export function refreshAllPlugins(force = false) {
  for (const m of machines()) if (reach[m.id] === 'up') void refreshPlugins(m, force);
}

/** After a change: the plugins again, and the providers (for cards and pages). */
async function after(m: Machine) {
  await refreshPlugins(m, true);
  about(m.host).then(a => { if (a.id === m.id) learn(m, a); }, () => {});
}

async function change(m: Machine, name: string, run: () => Promise<void>) {
  const key = `${m.id}/${name}`;
  if (changing[key]) return;
  setChanging(s => { s[key] = true; });
  setPluginError('');
  try {
    await run();
  } catch (e) {
    setPluginError(`${m.name}: ${(e as Error).message}`);
  } finally {
    setChanging(s => { delete s[key]; });
    await after(m);
  }
}

export const turn = (m: Machine, name: string, on: boolean) =>
  change(m, name, async () => { await post(m, { t: 'on', name, on }); });

/** Install a plugin on a machine from where another machine got it, or
 *  update it from there: after saying what it would run, since a plugin runs
 *  with peek's rights. */
export const fetchTo = (m: Machine, name: string, source: string, update: boolean) =>
  change(m, name, async () => {
    const d = await post(m, { t: 'inspect', source }, 120_000);
    if (d.name !== name) throw new Error(`that source is now ${String(d.name)}, not ${name}`);
    const missing = Array.isArray(d.missing) ? d.missing.map(String) : [];
    if (missing.length) throw new Error(`${name} needs ${missing.join(', ')}, which ${m.name} does not have`);
    const version = d.version ? ` ${String(d.version)}` : '';
    const ok = confirm(`${update ? 'Update' : 'Install'} ${name}${version} on ${m.name}?\n\n` +
      `It runs "${String(d.runs)}" there, with peek's rights on that machine (SYSTEM under the Windows ` +
      `service), so install only what you trust.\n\nFrom: ${source}`);
    if (!ok) return;
    await post(m, { t: update ? 'update' : 'install', id: d.id }, 120_000);
  });

// ---- what the overview shows ----

/** Newer first: numbers compared as numbers, anything else as text. */
export function compareVersions(a: string, b: string) {
  const pa = a.split(/[.+-]/), pb = b.split(/[.+-]/);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const x = pa[i] ?? '', y = pb[i] ?? '';
    const nx = Number(x), ny = Number(y);
    const c = x !== '' && y !== '' && !isNaN(nx) && !isNaN(ny) ? nx - ny : x.localeCompare(y);
    if (c) return c;
  }
  return 0;
}

/** A source another machine can fetch too: an https git address or an npm
 *  name, not a folder or a .tgz on the machine it came from. */
export const portable = (from?: string | null): from is string =>
  !!from && (from.startsWith('https://') || /^(@[a-z0-9._~-]+\/)?[a-z0-9._~-]+(@[^\s/\\]+)?$/i.test(from));

export interface Column {
  name: string;
  kind: string;
  description: string;
  /** The newest version a machine has of a plugin, and where it came from. */
  latest?: { version: string; from: string | null };
}

/** Every plugin any machine has: peek's own first, then the rest by name. */
export function columns(): Column[] {
  const all = new Map<string, Column>();
  for (const m of machines()) for (const i of m.plugins?.items ?? []) {
    const c = all.get(i.name) ?? { name: i.name, kind: i.kind, description: i.description };
    if (i.kind === PLUGIN && i.version) {
      const newer = !c.latest || compareVersions(i.version, c.latest.version) > 0;
      // The same version from a source others can fetch beats one they cannot.
      const better = c.latest && compareVersions(i.version, c.latest.version) === 0 && !portable(c.latest.from) && portable(i.from);
      if (newer || better) c.latest = { version: i.version, from: i.from ?? null };
    }
    if (!c.description && i.description) c.description = i.description;
    all.set(i.name, c);
  }
  const own = (c: Column) => c.kind === OWN ? 0 : 1;
  // The same object for an unchanged column, so lists keyed by it keep their rows.
  return [...all.values()].sort((a, b) => own(a) - own(b) || a.name.localeCompare(b.name)).map(c => {
    const was = seen.get(c.name);
    if (was && JSON.stringify(was) === JSON.stringify(c)) return was;
    seen.set(c.name, c);
    return c;
  });
}
const seen = new Map<string, Column>();

/** Where `source` can come from for this plugin, if any machine has one. */
export function sourceOf(name: string): string | null {
  for (const m of machines()) {
    const i = m.plugins?.items.find(i => i.name === name && i.kind === PLUGIN);
    if (i && portable(i.from)) return i.from;
  }
  return null;
}

/** What can be done with a plugin on a machine from here, if anything. */
export type Offer =
  | { t: 'toggle' }
  | { t: 'install'; source: string }
  | { t: 'update'; source: string; version: string };

export function offers(m: Machine, c: Column): Offer[] {
  const list = m.plugins;
  if (!list || list.viewOnly || reach[m.id] !== 'up' || pluginState[m.id] === 'old') return [];
  const item = list.items.find(i => i.name === c.name);
  const out: Offer[] = [];
  if (item) out.push({ t: 'toggle' });
  if (!list.remoteInstall || c.kind !== PLUGIN) return out;
  const source = c.latest && portable(c.latest.from) ? c.latest.from : sourceOf(c.name);
  if (!source) return out;
  if (!item) out.push({ t: 'install', source });
  else if (item.kind === PLUGIN && item.version && c.latest && compareVersions(c.latest.version, item.version) > 0) {
    out.push({ t: 'update', source, version: c.latest.version });
  }
  return out;
}

/** "2 h ago", for what a machine that is away last said. */
export function ago(at: number) {
  const s = Math.max(0, Math.round((Date.now() - at) / 1000));
  if (s < 90) return 'just now';
  const min = Math.round(s / 60);
  if (min < 90) return `${min} min ago`;
  const h = Math.round(min / 60);
  return h < 36 ? `${h} h ago` : `${Math.round(h / 24)} days ago`;
}
