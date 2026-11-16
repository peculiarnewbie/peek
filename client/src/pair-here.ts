// Pairing the client a machine serves itself: a one-time code from `peek pair`,
// or from Pair a device in another paired browser, traded for this browser's
// own token. (The hosted client pairs in hosted.ts.)
import { createSignal } from 'solid-js';
import { deviceLabel } from './hosted';
import { setMessage } from './state';
import { store } from './util';

/** Why this browser has to pair: it has no token, or its token stopped working. */
export const [pairing, setPairing] = createSignal<'' | 'needed' | 'wrong'>('');

/** What to call this machine when asking for a code: the name it gives,
 *  or else its address. */
export const [hostName, setHostName] = createSignal(location.hostname);
let askedName = false;
/** Ask the machine its name, once, when the prompt first shows: it answers
 *  that without a token. */
export function learnHostName() {
  if (askedName) return;
  askedName = true;
  fetch('/.well-known/peek', { cache: 'no-store' })
    .then(r => r.json())
    .then(a => { if (typeof a?.name === 'string' && a.name) setHostName(a.name); })
    .catch(() => {});
}

/** Trade `code` for a token and start again with it. Returns why it failed, or ''. */
export async function pairHere(code: string): Promise<string> {
  let r: Response;
  try {
    r = await fetch('/api/pair', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ code: code.trim(), label: deviceLabel() }),
      cache: 'no-store',
    });
  } catch {
    return `${hostName()} did not answer. Try again.`;
  }
  if (r.status === 403) return `That code is wrong, used or expired. Run \`peek pair\` on ${hostName()} for a new one.`;
  if (r.status === 429) return 'Too many wrong codes. Wait a minute and try again.';
  if (!r.ok) return `${hostName()} refused to pair (HTTP ${r.status}).`;
  const { token } = await r.json();
  store.set('token', token);
  location.reload();
  return '';
}

/** A link that ends in `#code=…`: pair with it, or ask for the code again. */
export function codeInUrl(): string {
  const m = /^#code=(.+)$/.exec(location.hash);
  return m ? decodeURIComponent(m[1]) : '';
}

export function pairFromHash() {
  const code = codeInUrl();
  history.replaceState(null, '', location.pathname);
  setMessage('Pairing…');
  void pairHere(code).then(why => {
    if (!why) return;
    setMessage('');
    setPairing('needed');
    setPairError(why);
  });
}

/** The last failure, shown under the code field. */
export const [pairError, setPairError] = createSignal('');
