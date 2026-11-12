// Sleep and wake for the hosted client's machines, through each host's
// power provider (`/api/power`).
//
// A sleeping machine runs nothing, so it is woken by another paired machine
// on its network, which sends it a Wake-on-LAN packet. What it takes to wake
// a machine (its network card and network) is kept here with the machine,
// from when it was last online. Whether waking really works also depends on
// its firmware, which nothing can read, so the first sleep is a test: the
// machine sleeps, and is woken again straight away.
import { createStore } from 'solid-js/store';
import { about, machines, reach, setReach, updateMachine, type Machine } from './hosted';

export interface Power {
  name: string;
  /** The machine can be put to sleep. */
  sleep: boolean;
  /** 'modern': Modern Standby, which may turn the network card off while asleep. */
  standby: 's3' | 'modern' | null;
  /** The network card to wake it through, or null (see `why`). */
  wake: { mac: string; addr: string; adapter: string; armed: boolean } | null;
  /** Why it cannot be woken, if it cannot. */
  why: string | null;
  /** Its networks, as address/prefix: which machines it can wake. */
  lan: string[];
  /** How the last try at waking it went. */
  tested: { ok: boolean; at: number } | null;
}

/** Time from sleeping to the test's wake: long enough to be well asleep. */
const TEST_WAKE_AFTER = 15000;
const ASLEEP_WITHIN = 60000;
const AWAKE_WITHIN = 90000;
const POLL = 2000;

async function request(m: Machine, body: object, ms = 12000): Promise<any> {
  const r = await fetch(`https://${m.host}/api/power`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ ...body, token: m.token }),
    cache: 'no-store',
    signal: AbortSignal.timeout(ms),
  });
  const answer = await r.json().catch(() => ({ t: 'error', why: `HTTP ${r.status}` }));
  if (answer.t === 'error') throw new Error(answer.why);
  return answer;
}

/** Keep what a machine says it can do, while it is online. A wake that
 *  failed since it was last online is reported to it now. */
export async function refreshPower(m: Machine) {
  try {
    if (m.wakeFailed && m.power?.wake) await request(m, { t: 'tested', ok: false });
    const power: Power = await request(m, { t: 'info' }, 6000);
    const { t: _, ...info } = power as Power & { t: string };
    if (JSON.stringify(info) !== JSON.stringify(m.power) || m.wakeFailed) {
      updateMachine(m.id, { power: info, wakeFailed: undefined });
    }
  } catch {}   // a host before 0.3.0, or gone again
}

const net = (cidr: string) => {
  const [ip, p] = cidr.split('/');
  const n = ip.split('.').reduce((a, o) => (a << 8 | +o) >>> 0, 0);
  const prefix = +p;
  const mask = prefix ? (~0 << (32 - prefix)) >>> 0 : 0;
  return { net: (n & mask) >>> 0, prefix };
};
const sameNetwork = (a: string, b: string) => {
  const x = net(a), y = net(b);
  return x.prefix === y.prefix && x.net === y.net;
};

/** An online machine on `m`'s network that can wake it. */
export function waker(m: Machine): Machine | undefined {
  const addr = m.power?.wake?.addr;
  if (!addr) return;
  return machines().find(o => o.id !== m.id && reach[o.id] === 'up' && o.power?.lan.some(n => sameNetwork(n, addr)));
}

/** What the card offers for sleep: nothing (with why), a test, or sleep. */
export type Offer = { kind: 'none'; why: string } | { kind: 'test'; by: Machine } | { kind: 'sleep'; by: Machine };
export function offer(m: Machine): Offer | null {
  const p = m.power;
  if (!p || !p.sleep) return null;
  if (!p.wake) return { kind: 'none', why: p.why ?? 'It cannot be woken over the network.' };
  if (!p.wake.armed) {
    return { kind: 'none', why: `${p.why}. To allow it, run in an administrator terminal there: ` +
      `powercfg /deviceenablewake "${p.wake.adapter}"` };
  }
  const by = waker(m);
  if (!by) return { kind: 'none', why: 'No other paired machine on its network is online to wake it.' };
  return p.tested?.ok ? { kind: 'sleep', by } : { kind: 'test', by };
}

// ---- what is under way, per machine ----
export interface Doing { text: string; busy: boolean; error?: boolean }
export const [doing, setDoing] = createStore<Record<string, Doing | undefined>>({});
const say = (m: Machine, text: string, busy = true, error = false) => setDoing(m.id, { text, busy, error });
/** Clears a finished message after a while, unless something newer replaced it. */
const settle = (m: Machine, ms = 8000) => {
  const now = doing[m.id];
  setTimeout(() => { if (doing[m.id] === now) setDoing(m.id, undefined); }, ms);
};

const pause = (ms: number) => new Promise(r => setTimeout(r, ms));

async function online(m: Machine) {
  try {
    const a = await about(m.host, 3000);
    return a.id === m.id;
  } catch { return false; }
}

/** Poll until the machine is (or is not) online, or the time is up. */
async function until(m: Machine, up: boolean, within: number) {
  const end = Date.now() + within;
  while (Date.now() < end) {
    if (await online(m) === up) { setReach(m.id, up ? 'up' : 'down'); return true; }
    await pause(POLL);
  }
  return false;
}

function failedToWake(m: Machine, by: Machine) {
  const p = m.power!;
  updateMachine(m.id, { wakeFailed: true });
  say(m, `${m.name} did not wake. Most often Wake-on-LAN is off in its BIOS or UEFI settings ("Wake on LAN", ` +
    `"Power on by PCI-E"), or "Wake on Magic Packet" is off for ${p.wake!.adapter} in Windows' Device Manager.` +
    (p.standby === 'modern' ? ' It also uses Modern Standby, which can turn the network card off while asleep.' : '') +
    ` Press its power button, then test again. (${by.name} sent the packet.)`, false, true);
}

export async function sleep(m: Machine, test: boolean) {
  const by = waker(m);
  if (!by) return;
  say(m, 'Going to sleep…');
  try {
    await request(m, { t: 'sleep' });
  } catch (e) {
    say(m, `It did not go to sleep: ${(e as Error).message}`, false, true);
    return;
  }
  if (!await until(m, false, ASLEEP_WITHIN)) {
    say(m, `${m.name} said it would sleep, but is still online.`, false, true);
    return;
  }
  if (!test) {
    say(m, `Asleep. ${by.name} can wake it.`, false);
    settle(m);
    return;
  }
  say(m, `Asleep. Waking it from ${by.name} in ${TEST_WAKE_AFTER / 1000} s…`);
  await pause(TEST_WAKE_AFTER);
  await wake(m, true);
}

export async function wake(m: Machine, test = false) {
  const by = waker(m);
  if (!by || !m.power?.wake) {
    say(m, 'No other paired machine on its network is online to wake it.', false, true);
    return;
  }
  const start = Date.now();
  try {
    await request(by, { t: 'wake', mac: m.power.wake.mac, addr: m.power.wake.addr });
  } catch (e) {
    say(m, `${by.name} could not send the wake: ${(e as Error).message}`, false, true);
    return;
  }
  say(m, `Waking, sent from ${by.name}…`);
  if (!await until(m, true, AWAKE_WITHIN)) {
    failedToWake(m, by);
    return;
  }
  const took = Math.round((Date.now() - start) / 1000);
  if (test) {
    try { await request(m, { t: 'tested', ok: true }); } catch {}
  }
  await refreshPower(machines().find(x => x.id === m.id) ?? m);
  say(m, test ? `Waking works: it was back after ${took} s. Sleep is ready.` : `Awake after ${took} s.`, false);
  settle(m);
}
