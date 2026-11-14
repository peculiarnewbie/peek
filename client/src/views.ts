import { createSignal } from 'solid-js';
// What a machine's providers ask to be shown, and asking them.
//
// A provider is a program on a machine, beside its screen: peek's own
// (vitals, power) or one added there. It says what to show and this page
// draws it, from a few stock pieces: a line of text, a button, tiles,
// meters. No code of a provider's runs here, and this page knows nothing of
// what any of them is for. docs/providers.md has the whole of it.
//
// Views have a place: `card`, on a machine's card in the list (asked for by
// request, every so often), and `page`, on the machine's own page (sent on
// its socket for as long as it is watched).
//
// Two things are provisional, and may change with the second provider to
// use them:
// - Records. A view can come with a record: what its provider lets the same
//   provider on the other paired machines know of this one. This browser
//   is the only thing paired with them all, so it carries the records, and
//   keeps them for while a machine is away.
// - Items about another machine. A view's item can name another machine,
//   and is shown on that one's card. Pressing its button still asks the
//   machine that sent it.
import { createStore, reconcile } from 'solid-js/store';
import { machines, reach, updateMachine, type Machine } from './hosted';

/** A line of text: plain, quieter (`hint`), or something that went wrong. */
export interface TextItem { kind: 'text'; text: string; tone?: 'hint' | 'error'; busy?: boolean; about?: string }
/** A button. Pressed, its `do` goes back to the provider that sent it. */
export interface ButtonItem {
  kind: 'button';
  label: string;
  /** Said beside it, quietly. */
  note?: string;
  style?: 'primary';
  /** Asked before anything happens. */
  confirm?: string;
  do: unknown;
  /** Asked of the same provider on another machine first; if that one
   *  refuses, `do` is not sent. (Provisional.) */
  first?: { peer: string; do: unknown };
  about?: string;
}
/** Large figures, each with a label: "12 %". */
export interface TilesItem { kind: 'tiles'; tiles: { label: string; value: string }[]; about?: string }
/** Bars from 0 to 1, each with a label and what it stands for. */
export interface MetersItem { kind: 'meters'; meters: { label: string; fraction: number; text: string }[]; about?: string }
export type Item = TextItem | ButtonItem | TilesItem | MetersItem;

export interface View { title?: string; aside?: string; items: Item[] }

/** What a machine says of one of its providers (`/.well-known/peek`). */
export interface ProviderAbout {
  name: string;
  /** Where it has something to show. */
  places?: string[];
  /** It wants the records of the same provider on the other machines. */
  peers?: boolean;
  /** It has a page of its own, which the machine serves (Frame.tsx). */
  frame?: boolean;
}

/** Descriptor for the client served by a machine itself. Refresh after a
 * management page changes providers, just as the hosted client does. */
export const [own, setOwn] = createSignal<{ providers?: ProviderAbout[]; version?: string; update?: string | null } | null>(null);
export async function refreshOwn() {
  try { const r = await fetch('/.well-known/peek', { cache: 'no-store' }); if (r.ok) setOwn(await r.json()); }
  catch { /* the main connection reports availability */ }
}

export const frameProviders = (m: { providers?: ProviderAbout[] } | null | undefined) =>
  (m?.providers ?? []).filter(p => p.frame).map(p => p.name);

interface Answer { t: string; why?: string; place?: string; view?: View; record?: unknown; again?: number | null }

/** Anything may come from a provider: keep what can be drawn. */
function clean(view: unknown): View {
  const v = (view ?? {}) as Partial<View>;
  const items = (Array.isArray(v.items) ? v.items : []).filter((i): i is Item =>
    !!i && typeof i === 'object' && (
      (i.kind === 'text' && typeof i.text === 'string') ||
      (i.kind === 'button' && typeof i.label === 'string') ||
      (i.kind === 'tiles' && Array.isArray(i.tiles)) ||
      (i.kind === 'meters' && Array.isArray(i.meters))));
  return {
    title: typeof v.title === 'string' ? v.title : undefined,
    aside: typeof v.aside === 'string' ? v.aside : undefined,
    items,
  };
}

const withPlace = (m: Machine, place: string) => (m.providers ?? []).filter(p => p.places?.includes(place));

async function ask(m: Machine, provider: string, body: object, ms = 12000): Promise<Answer> {
  const r = await fetch(`https://${m.host}/api/provider/${encodeURIComponent(provider)}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ ...body, token: m.token }),
    cache: 'no-store',
    signal: AbortSignal.timeout(ms),
  });
  const answer: Answer = await r.json().catch(() => ({ t: 'error', why: `HTTP ${r.status}` }));
  if (answer.t === 'error') throw new Error(answer.why ?? 'it refused');
  return answer;
}

/** The other machines with this provider, as it is told of them. */
function peersOf(m: Machine, provider: ProviderAbout) {
  if (!provider.peers) return undefined;
  return machines()
    .filter(o => o.id !== m.id && (o.records?.[provider.name] !== undefined || o.providers?.some(p => p.name === provider.name)))
    .map(o => ({ id: o.id, name: o.name, up: reach[o.id] === 'up', record: o.records?.[provider.name] ?? null }));
}

// ---- cards ----

/** Each machine's card views, by provider. Gone when the machine is. */
const [cards, setCards] = createStore<Record<string, Record<string, View>>>({});
/** What went wrong with the last button pressed on a machine's card. */
export const [cardError, setCardError] = createStore<Record<string, string | undefined>>({});
/** A button pressed on a machine's card is still being answered. */
export const [pressing, setPressing] = createStore<Record<string, boolean | undefined>>({});

let againTimer: ReturnType<typeof setTimeout> | undefined;
let refresh: () => void = () => {};
/** What the list calls to look at every machine again (its probe). */
export function onAgain(run: () => void) { refresh = run; }

/** A provider with something under way asks to be asked again soon. */
function again(seconds: number | null | undefined) {
  if (!seconds || seconds <= 0) return;
  clearTimeout(againTimer);
  againTimer = setTimeout(() => refresh(), Math.max(1, Math.min(seconds, 60)) * 1000);
}

function keep(m: Machine, provider: string, answer: Answer) {
  if (!cards[m.id]) setCards(m.id, {});
  setCards(m.id, provider, reconcile(clean(answer.view)));
  if (answer.record !== undefined) {
    const now = machines().find(x => x.id === m.id);
    if (now && JSON.stringify(now.records?.[provider]) !== JSON.stringify(answer.record)) {
      updateMachine(m.id, { records: { ...now.records, [provider]: answer.record } });
      // The others were asked without knowing this: ask them again.
      again(1);
    }
  }
  again(answer.again);
}

/** Ask a machine that is up for its card views. */
export function refreshCards(m: Machine) {
  for (const p of withPlace(m, 'card')) {
    ask(m, p.name, { t: 'view', place: 'card', peers: peersOf(m, p) }, 6000)
      .then(answer => keep(m, p.name, answer))
      .catch(() => { if (cards[m.id]) setCards(m.id, p.name, undefined!); });
  }
}

/** A machine went away: what it had on the cards goes with it. */
export function dropCards(id: string) {
  setCards(id, undefined!);
}

export interface Shown { item: Item; from: Machine; provider: string }

/** What a machine's card shows: its own providers' items while it is up,
 *  and what the others' say about it (from the first that says anything). */
export function cardItems(m: Machine): Shown[] {
  const shown: Shown[] = [];
  const names = new Set<string>();
  for (const o of machines()) for (const name of Object.keys(cards[o.id] ?? {})) names.add(name);
  for (const provider of [...names].sort()) {
    for (const item of cards[m.id]?.[provider]?.items ?? []) {
      if (!item.about) shown.push({ item, from: m, provider });
    }
    for (const o of machines()) {
      if (o.id === m.id) continue;
      const about = (cards[o.id]?.[provider]?.items ?? []).filter(i => i.about === m.id);
      if (about.length) {
        shown.push(...about.map(item => ({ item, from: o, provider })));
        break;
      }
    }
  }
  return shown;
}

/** A button on `on`'s card was pressed. */
export async function press(on: Machine, shown: Shown) {
  const { from, provider } = shown;
  const item = shown.item as ButtonItem;
  if (pressing[on.id] || (item.confirm && !confirm(item.confirm))) return;
  setCardError(on.id, undefined);
  setPressing(on.id, true);
  try {
    if (item.first) {
      const peer = machines().find(o => o.id === item.first!.peer);
      if (!peer) throw new Error('the machine it needs is no longer paired here');
      try {
        await ask(peer, provider, { t: 'peer', from: { id: from.id, name: from.name }, do: item.first.do });
      } catch (e) {
        throw new Error(`${peer.name} could not help: ${(e as Error).message}`);
      }
    }
    const about = from.providers?.find(p => p.name === provider);
    keep(from, provider, await ask(from, provider, { t: 'do', do: item.do, peers: about && peersOf(from, about) }));
  } catch (e) {
    setCardError(on.id, `${item.label}: ${(e as Error).message}`);
  } finally {
    setPressing(on.id, false);
  }
  // The others see what changed.
  again(1);
}

// ---- the page ----

/** The page's views, by provider: for the machine this page is connected to. */
export const [pages, setPages] = createStore<Record<string, View>>({});

/** A provider's message on the socket. */
export function fromProvider(provider: string, m: Answer) {
  if (m?.t === 'view' && m.place === 'page') setPages(provider, reconcile(clean(m.view)));
}

export function dropPages() {
  setPages(reconcile({}));
}

/** The providers with something for the page, on the machine this page is
 *  connected to. */
export const pageProviders = (m: { providers?: ProviderAbout[] } | null | undefined) =>
  (m?.providers ?? []).filter(p => p.places?.includes('page')).map(p => p.name);
