// A provider's own page, shown in a frame (a spike: see docs/frames.md):
// in a panel over the screen, or in place of it at /screen/<machine>/<provider>.
//
// The page is served by the machine (/p/<name>/), so it runs as the
// machine's origin and never as this client's: it cannot read this page or
// its storage, where the tokens are. It gets a ticket for its provider, not
// a token. Its box is always drawn with the provider's and the machine's
// names, so nothing in it can pass for this client.
import { createEffect, createMemo, createSignal, For, onCleanup, Show, type Accessor } from 'solid-js';
import { machine, send } from '../connection';
import { about, current, home, learn, navigate, pagePath, screenPath, shownPage, HOSTED } from '../hosted';
import { active, live, setOnPage } from '../state';
import { frameProviders, refreshOwn } from '../views';
import { Icon } from './Icons';

/** The provider whose page is open in the panel, if any. */
export const [framed, setFramed] = createSignal<string | null>(null);

/** Open a provider's page: in place of the screen on the hosted client, in a panel otherwise. */
export function openPage(name: string) {
  const m = current();
  if (HOSTED && m) navigate(pagePath(m, name));
  else setFramed(framed() === name ? null : name);
}

/** What the frame may do. Not: navigate this tab, open windows, or show dialogs. */
const SANDBOX = 'allow-scripts allow-same-origin allow-forms allow-downloads';
/** Browser features handed to it, each on purpose. */
const ALLOW = 'fullscreen; clipboard-write';
/** The colours sent to the page, so it can look like the client. */
const THEME = ['--page', '--surface', '--raised', '--ink', '--ink-2', '--muted', '--line', '--s1', '--good', '--warning', '--sans'];

interface Opened { url: string; origin: string; machine: string }

/** A ticket for `name`'s page on the machine shown now, and the address to frame. */
function useTicket(name: Accessor<string | null>) {
  const [src, setSrc] = createSignal<Opened | null>(null);
  const [error, setError] = createSignal('');
  const [revision, setRevision] = createSignal(0);
  const renew = () => setRevision(n => n + 1);
  let failures = 0;
  let previous = '';
  // Only a change of page or machine opens it again, not a change in what is
  // kept about the machine (its providers, say).
  const target = createMemo(() => {
    const p = name(), m = machine();
    return p && m ? { p, ...m } : null;
  }, null, { equals: (a, b) => JSON.stringify(a) === JSON.stringify(b) });
  createEffect(() => {
    const t = target();
    const connected = live();
    revision();
    if (!t) { setSrc(null); setError(''); return; }
    // The host may have restarted and lost its in-memory tickets. Restate
    // authorization after the main socket reconnects, without a manual reload.
    if (!connected) return;
    setError('');
    const { p, ...m } = t;
    const identity = `${m.base}/${p}`;
    if (identity !== previous) { previous = identity; failures = 0; setSrc(null); }
    const ctl = new AbortController();
    let retry: ReturnType<typeof setTimeout> | undefined;
    onCleanup(() => { ctl.abort(); clearTimeout(retry); });
    fetch(`${m.base}/api/frame/${encodeURIComponent(p)}`, {
      method: 'POST', headers: { 'content-type': 'application/json' }, signal: ctl.signal,
      body: JSON.stringify({ token: m.token }), cache: 'no-store',
    })
      .then(r => r.ok ? r.json() : r.json().then(b => Promise.reject(Object.assign(new Error(b.error || `HTTP ${r.status}`), { retryable: r.status === 404 || r.status >= 500 }))))
      .then(({ ticket, path }) => {
        if (ctl.signal.aborted) return;
        failures = 0;
        setSrc({ url: `${m.base}${path}#ticket=${ticket}`, origin: new URL(m.base).origin, machine: m.name });
      })
      .catch(e => {
        if (ctl.signal.aborted) return;
        setError(e.message === 'no page' ? `${m.name} has no page called ${p}.` : `Cannot open its page: ${e.message}`);
        // A host can accept connections before its provider has announced
        // its page. Bound retries; an uninstalled page must not poll forever.
        if (e.retryable !== false && ++failures <= 10) retry = setTimeout(renew, 1000);
      });
  });
  return { src, error, renew };
}

function PageFrame(props: { name: string; src: Opened; renew: () => void }) {
  let frame!: HTMLIFrameElement;
  // A page may say the machine's providers changed (the plugins page, after
  // an install): look at them again, now and once its providers have started.
  const onMessage = (e: MessageEvent) => {
    if (e.source !== frame.contentWindow || e.origin !== props.src.origin) return;
    // A page can request a fresh ticket, never obtain the machine's token.
    if (e.data?.t === 'renew-ticket') { props.renew(); return; }
    if (e.data?.t !== 'changed') return;
    const id = current()?.id;
    for (const wait of [0, 1500]) setTimeout(() => {
      // The machine as it is kept now, not as it was a moment ago.
      if (!HOSTED) { void refreshOwn(); return; }
      const m = current();
      if (m && m.id === id) about(m.host).then(a => { const now = current(); if (now && a.id === now.id) learn(now, a); }, () => {});
    }, wait);
  };
  addEventListener('message', onMessage);
  onCleanup(() => removeEventListener('message', onMessage));
  function sendTheme() {
    const css = getComputedStyle(document.documentElement);
    const vars = Object.fromEntries(THEME.map(k => [k, css.getPropertyValue(k).trim()]));
    // Only to the machine's origin: if the frame went elsewhere, it gets nothing.
    frame.contentWindow?.postMessage({ t: 'theme', vars }, props.src.origin);
  }
  return <iframe ref={frame} src={props.src.url} sandbox={SANDBOX} allow={ALLOW}
    referrerPolicy="no-referrer" title={`${props.name} from ${props.src.machine}`} onLoad={sendTheme} />;
}

/** The panel over the screen. */
export function Frame() {
  const { src, error, renew } = useTicket(framed);
  return (
    <Show when={framed()}>
      <div id="frame" class="panel" role="dialog" aria-label={`${framed()}, a provider's page`}>
        <div id="frame-head">
          <span id="frame-title">{framed()}</span>
          <span id="frame-from">{src() ? `a page from ${src()!.machine}` : 'opening…'}</span>
          <button class="tb icon" title="Close" onClick={() => setFramed(null)}><Icon name="x" /></button>
        </div>
        <Show when={src()} keyed fallback={<div id="frame-wait">{error() || 'Opening…'}</div>}>
          {opened => <PageFrame name={framed()!} src={opened} renew={renew} />}
        </Show>
      </div>
    </Show>
  );
}

/** A provider's page in place of the screen, with a tab for each and one back to the screen. */
export function ProviderPage() {
  const name = () => HOSTED ? shownPage() : null;
  const { src, error, renew } = useTicket(name);
  // The machine stops sending the screen while it is not shown.
  createEffect(() => {
    setOnPage(!!name() || !!framed());
    send({ t: 'vis', v: active() });
  });
  return (
    <Show when={name() && current()}>
      <div id="ppage">
        <div id="ppage-head">
          <button class="tb" title="All machines" onClick={home}><Icon name="back" /><span>{current()!.name}</span></button>
          <div id="ppage-tabs" role="tablist">
            <button class="ptab" role="tab" onClick={() => navigate(screenPath(current()!))}><Icon name="monitor" />Screen</button>
            <For each={frameProviders(current())}>{p =>
              <button class="ptab" role="tab" aria-selected={p === name()} onClick={() => navigate(pagePath(current()!, p))}>{p}</button>
            }</For>
          </div>
          <span id="ppage-from">{src() ? `drawn by ${name()} on ${src()!.machine}` : ''}</span>
        </div>
        <Show when={src()} keyed fallback={<div id="frame-wait">{error() || 'Opening…'}</div>}>
          {opened => <PageFrame name={name()!} src={opened} renew={renew} />}
        </Show>
      </div>
    </Show>
  );
}
