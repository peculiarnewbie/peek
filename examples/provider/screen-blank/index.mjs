// Blank a laptop's backlight while using peek, and optionally power off the
// display via Niri when nobody is viewing peek so the GPU/display pipeline can
// sleep. Restores the saved brightness when turned off.
//
// Install:
//   peek plugin add --link examples/provider/screen-blank

import { execFileSync } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const OPEN = 1, CLOSE = 2, TEXT = 3, LOG = 6, ABOUT = 7;

const viewOnly = process.env.PEEK_VIEW_ONLY === '1';
const dataDir = process.env.PEEK_DATA_DIR || '';
const stateFile = dataDir ? join(dataDir, 'state.json') : '';

function parseSettings() {
  try {
    const raw = JSON.parse(process.env.PEEK_SETTINGS || '{}');
    const idle = Number(raw.idle_seconds);
    return {
      dpms: raw.dpms !== false,
      idleMs: Number.isFinite(idle) && idle >= 0 ? Math.round(idle * 1000) : 5000,
      device: typeof raw.device === 'string' ? raw.device.trim() : '',
    };
  } catch {
    return { dpms: true, idleMs: 5000, device: '' };
  }
}

const settings = parseSettings();

function send(kind, channel, payload) {
  const bytes = typeof payload === 'string'
    ? Buffer.from(payload)
    : Buffer.from(JSON.stringify(payload));
  const head = Buffer.alloc(13);
  head.writeUInt32LE(bytes.length, 0);
  head.writeUInt8(kind, 4);
  head.writeBigUInt64LE(channel, 5);
  process.stdout.write(Buffer.concat([head, bytes]));
}

function log(line) {
  send(LOG, 0n, `screen-blank: ${line}`);
}

function loadState() {
  if (!stateFile) return { blanked: false, savedBrightness: null };
  try {
    const parsed = JSON.parse(readFileSync(stateFile, 'utf8'));
    return {
      blanked: !!parsed.blanked,
      savedBrightness: typeof parsed.savedBrightness === 'number' && parsed.savedBrightness > 0
        ? parsed.savedBrightness
        : null,
    };
  } catch {
    return { blanked: false, savedBrightness: null };
  }
}

function saveState() {
  if (!stateFile) return;
  try {
    mkdirSync(dataDir, { recursive: true });
    writeFileSync(stateFile, JSON.stringify({
      blanked: state.blanked,
      savedBrightness: state.savedBrightness,
    }));
  } catch (e) {
    log(`could not save state: ${e.message}`);
  }
}

function brightnessArgs(...rest) {
  return settings.device
    ? ['-d', settings.device, ...rest]
    : ['-c', 'backlight', ...rest];
}

function readBrightness() {
  try {
    const cur = Number(execFileSync('brightnessctl', brightnessArgs('get'), { encoding: 'utf8', timeout: 3000 }).trim());
    const max = Number(execFileSync('brightnessctl', brightnessArgs('max'), { encoding: 'utf8', timeout: 3000 }).trim());
    if (!Number.isFinite(cur) || !Number.isFinite(max) || max <= 0) return null;
    return { cur, max, pct: Math.round((cur / max) * 100) };
  } catch {
    return null;
  }
}

function setBrightness(value) {
  execFileSync('brightnessctl', brightnessArgs('set', String(value), '-q'), { timeout: 3000 });
}

function niriAction(action) {
  if (!settings.dpms) return;
  try {
    execFileSync('niri', ['msg', 'action', action], { timeout: 3000, stdio: 'ignore' });
  } catch (e) {
    log(`niri ${action} failed: ${e.message}`);
  }
}

function findInternalOutput() {
  try {
    const raw = execFileSync('niri', ['msg', '-j', 'outputs'], { encoding: 'utf8', timeout: 3000 });
    const outputs = JSON.parse(raw);
    const names = Object.keys(outputs);
    const name = names.find(n => /^eDP-/i.test(n)) || names[0];
    return name ? { name, off: outputs[name]?.current_mode == null } : null;
  } catch (e) {
    log(`niri outputs failed: ${e.message}`);
    return null;
  }
}

function wakeDisplay(keepDark) {
  if (!settings.dpms) return;
  const out = findInternalOutput();
  if (out && out.off) {
    try {
      execFileSync('niri', ['msg', 'output', out.name, 'on'], { timeout: 3000, stdio: 'ignore' });
    } catch (e) {
      log(`niri output ${out.name} on failed: ${e.message}`);
    }
  }
  niriAction('power-on-monitors');
  if (keepDark) {
    try { setBrightness(0); } catch (e) { log(`brightness 0 failed: ${e.message}`); }
  }
}

function sleepDisplay() {
  if (!settings.dpms) return;
  const out = findInternalOutput();
  if (out) {
    try {
      execFileSync('niri', ['msg', 'output', out.name, 'off'], { timeout: 3000, stdio: 'ignore' });
      return;
    } catch (e) {
      log(`niri output ${out.name} off failed: ${e.message}`);
    }
  }
  niriAction('power-off-monitors');
}

const state = loadState();
let screenActive = false;
let displayAsleep = false;
let idleTimer = null;
const pageChannels = new Set();

function clearIdleTimer() {
  if (idleTimer) {
    clearTimeout(idleTimer);
    idleTimer = null;
  }
}

function scheduleSleep() {
  clearIdleTimer();
  if (!state.blanked || !settings.dpms || screenActive) return;
  if (settings.idleMs === 0) {
    sleepDisplay();
    displayAsleep = true;
    pushPages();
    return;
  }
  idleTimer = setTimeout(() => {
    idleTimer = null;
    if (state.blanked && !screenActive) {
      sleepDisplay();
      displayAsleep = true;
      pushPages();
    }
  }, settings.idleMs);
}

function applyBlank(on) {
  clearIdleTimer();
  if (on) {
    const info = readBrightness();
    if (info && info.cur > 0) state.savedBrightness = info.cur;
    setBrightness(0);
    state.blanked = true;
    saveState();
    if (settings.dpms && !screenActive) {
      sleepDisplay();
      displayAsleep = true;
    } else {
      wakeDisplay(true);
      displayAsleep = false;
    }
    log(`blanked (saved=${state.savedBrightness ?? 'default'}, dpms=${settings.dpms}, active=${screenActive})`);
  } else {
    const target = state.savedBrightness && state.savedBrightness > 0 ? state.savedBrightness : '50%';
    wakeDisplay(false);
    setBrightness(target);
    state.blanked = false;
    state.savedBrightness = null;
    displayAsleep = false;
    saveState();
    log(`restored (brightness=${target})`);
  }
  pushPages();
}

function onScreenEvent(mode) {
  const active = mode === 'active';
  if (active === screenActive) return;
  screenActive = active;
  if (!state.blanked) return;
  if (active) {
    clearIdleTimer();
    try { setBrightness(0); } catch (e) { log(`brightness 0 failed: ${e.message}`); }
    wakeDisplay(true);
    displayAsleep = false;
    pushPages();
  } else {
    scheduleSleep();
    pushPages();
  }
}

function view(place) {
  const info = readBrightness();
  if (state.blanked) {
    const note = !settings.dpms
      ? 'backlight off'
      : displayAsleep
        ? 'dark · display asleep'
        : screenActive
          ? 'dark · viewing'
          : 'dark · sleeping soon';
    const items = [
      { kind: 'button', label: 'Restore screen', note, style: 'primary', do: { t: 'blank', on: false } },
    ];
    return {
      t: 'view',
      place,
      view: place === 'page' ? { title: 'Laptop screen', aside: 'dark', items } : { items },
    };
  }
  const note = info ? `${info.pct}% backlight` : 'backlight on';
  const items = [
    { kind: 'button', label: 'Blank screen', note, do: { t: 'blank', on: true } },
  ];
  return {
    t: 'view',
    place,
    view: place === 'page' ? { title: 'Laptop screen', aside: note, items } : { items },
  };
}

function pushPages() {
  if (pageChannels.size === 0) return;
  const msg = view('page');
  for (const ch of pageChannels) send(TEXT, ch, msg);
}

function answer(channel, ask) {
  if (channel === 0n && ask.t === 'event' && (ask.screen === 'active' || ask.screen === 'idle')) {
    onScreenEvent(ask.screen);
    return null;
  }
  if (ask.t === 'view' && (ask.place === 'card' || ask.place === 'page')) {
    if (ask.place === 'page') pageChannels.add(channel);
    return view(ask.place);
  }
  if (ask.t === 'do' && ask.do?.t === 'blank') {
    if (viewOnly) return { t: 'error', why: 'this machine is view-only' };
    try {
      applyBlank(!!ask.do.on);
      return view(pageChannels.has(channel) ? 'page' : 'card');
    } catch (e) {
      return { t: 'error', why: e.message || 'could not change backlight' };
    }
  }
  return { t: 'error', why: 'screen-blank does not know that' };
}

send(ABOUT, 0n, { places: ['card', 'page'], events: ['screen'] });

let pending = Buffer.alloc(0);
process.stdin.on('data', chunk => {
  pending = Buffer.concat([pending, chunk]);
  while (pending.length >= 13) {
    const length = pending.readUInt32LE(0);
    if (pending.length < 13 + length) break;
    const kind = pending.readUInt8(4);
    const channel = pending.readBigUInt64LE(5);
    const payload = pending.subarray(13, 13 + length).toString();
    pending = pending.subarray(13 + length);
    if (kind === CLOSE) {
      pageChannels.delete(channel);
      continue;
    }
    if (kind !== TEXT) continue;
    let ask;
    try { ask = JSON.parse(payload); } catch { continue; }
    const reply = answer(channel, ask);
    if (reply) send(TEXT, channel, reply);
  }
});

process.stdin.on('end', () => process.exit(0));
