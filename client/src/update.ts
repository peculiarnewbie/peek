// Ask a host to check for updates, update itself, or restart.
import { createStore } from 'solid-js';
import type { About } from './hosted';

/** What the host is doing, by its address, while the request runs. */
export const [updating, setUpdating] = createStore<Record<string, string | undefined>>({});

const BACK_WITHIN = 90_000;
type Checked = { t: 'checked'; version: string; latest: string; update: string | null };
type Answer = Checked
  | { t: 'updated'; to: string; instance?: string | null }
  | { t: 'current' }
  | { t: 'restarting'; instance?: string | null }
  | { t: 'error'; why?: string };

async function readHost(base: string): Promise<About & { instance?: string | null; update_check?: boolean }> {
  const r = await fetch(`${base}/.well-known/peek`, { cache: 'no-store', signal: AbortSignal.timeout(3000) });
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  return r.json();
}

async function request(base: string, path: 'update' | 'restart', body: { token: string; check?: true }, timeout: number): Promise<Answer> {
  const r = await fetch(`${base}/api/${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
    cache: 'no-store',
    signal: AbortSignal.timeout(timeout),
  });
  const answer: Answer = await r.json().catch(() => ({ t: 'error', why: `HTTP ${r.status}` }));
  if (answer.t === 'error') throw new Error(answer.why ?? 'the host refused');
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  return answer;
}

async function progress(base: string, label: string, run: () => Promise<void>) {
  setUpdating(s => { s[base] = label; });
  try { await run(); }
  finally { setUpdating(s => { delete s[base]; }); }
}

async function waitForHost(base: string, instance: string | null | undefined, version?: string): Promise<About> {
  const until = Date.now() + BACK_WITHIN;
  while (Date.now() < until) {
    await new Promise(done => setTimeout(done, 2000));
    try {
      const about = await readHost(base);
      if (typeof about.protocol === 'number' && (!instance || about.instance !== instance)
        && (!version || about.version === version)) return about;
    } catch { /* still restarting */ }
  }
  throw new Error(version ? `the host has not come back on ${version} yet` : 'the host has not come back yet');
}

export function checkUpdate(base: string, token: string, onResult: (checked: Checked) => void) {
  return progress(base, 'Checking for updates…', async () => {
    // Older hosts ignore the check flag and would install the update.
    if ((await readHost(base)).update_check !== true) throw new Error('the host does not support checking for updates');
    const answer = await request(base, 'update', { token, check: true }, 130_000);
    if (answer.t !== 'checked') throw new Error('the host does not support checking for updates');
    onResult(answer);
  });
}

/** Give `seen` the host's new descriptor once it has restarted. */
export function upgrade(base: string, token: string, seen: (about: About) => void) {
  return progress(base, 'Updating…', async () => {
    const answer = await request(base, 'update', { token }, 200_000);
    if (answer.t === 'current') return;
    if (answer.t !== 'updated') throw new Error('unexpected update response');
    setUpdating(s => { s[base] = `Restarting on ${answer.to}…`; });
    seen(await waitForHost(base, answer.instance, answer.to));
  });
}

export function restartHost(base: string, token: string) {
  return progress(base, 'Restarting…', async () => {
    const answer = await request(base, 'restart', { token }, 10_000);
    if (answer.t !== 'restarting') throw new Error('unexpected restart response');
    await waitForHost(base, answer.instance);
  });
}
