// Updating a machine's peek from the client: the machine updates itself to
// the newest build on its channel and restarts on it (POST /api/update), then
// answers again on the new version.
import { createStore } from 'solid-js/store';

/** What an update is doing, by the machine's address, while it runs. */
export const [updating, setUpdating] = createStore<Record<string, string | undefined>>({});

/** How long a machine may take to come back on the new version. */
const BACK_WITHIN = 90_000;

/** Ask the machine at `base` (its address, or '' for the one that served this
 *  page) to update, and wait until it answers on the new version, which
 *  `seen` is given. Throws with what went wrong. */
export async function upgrade(base: string, token: string, seen: (about: Record<string, unknown>) => void) {
  setUpdating(base, 'Updating…');
  try {
    const r = await fetch(`${base}/api/update`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ token }),
      cache: 'no-store',
      // Downloading and checking the new version comes first.
      signal: AbortSignal.timeout(180_000),
    });
    const answer = await r.json().catch(() => ({ t: 'error', why: `HTTP ${r.status}` }));
    if (answer.t === 'error') throw new Error(answer.why ?? 'it refused');
    if (answer.t !== 'updated') return;
    setUpdating(base, `Restarting on ${answer.to}…`);
    const until = Date.now() + BACK_WITHIN;
    while (Date.now() < until) {
      await new Promise(done => setTimeout(done, 2000));
      try {
        const r = await fetch(`${base}/.well-known/peek`, { cache: 'no-store', signal: AbortSignal.timeout(3000) });
        const about = await r.json();
        if (about.version === answer.to) { seen(about); return; }
      } catch { /* still restarting */ }
    }
    throw new Error(`it has not come back on ${answer.to} yet`);
  } finally {
    setUpdating(base, undefined);
  }
}
