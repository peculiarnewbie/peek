#!/usr/bin/env node
// Serves the hosted client from this machine's dev server, with hot reload,
// at https://dev.peek.peculiarnewbie.com, through a Cloudflare Tunnel. Every
// machine on a nightly trusts that site (default_web in src/main.rs), so a
// client change can be tried against all of them before it is published.
// The site is up only while this runs.
//
//   cd client && npm run dev:tunnel
//
// One machine serves it: the one holding the tunnel's credentials (for us,
// hp-book). Setting that machine up, once:
//   1. Install cloudflared (https://developers.cloudflare.com/cloudflare-one/
//      connections/connect-networks/downloads/).
//   2. cloudflared tunnel login       pick the zone peculiarnewbie.com
//   3. cloudflared tunnel create peek-dev
//   4. cloudflared tunnel route dns peek-dev dev.peek.peculiarnewbie.com
// The credentials stay in ~/.cloudflared on that machine; nothing else needs them.
import { spawn } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const TUNNEL = 'peek-dev';
const SITE = 'https://dev.peek.peculiarnewbie.com';
const PORT = 5173;
const client = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', 'client');

// Vite through Node: the Cloudflare plugin that the hosted mode loads does not run under Bun.
const vite = spawn(process.execPath,
  [path.join(client, 'node_modules', 'vite', 'bin', 'vite.js'), '--mode', 'hosted', '--host', '127.0.0.1', '--port', String(PORT), '--strictPort'],
  { cwd: client, stdio: 'inherit', env: { ...process.env, PEEK_CHANNEL: 'next' } });
const tunnel = spawn('cloudflared', ['tunnel', '--no-autoupdate', 'run', '--url', `http://127.0.0.1:${PORT}`, TUNNEL],
  { stdio: ['ignore', 'ignore', 'pipe'] });

let stopping = false;
function stop(code) {
  if (stopping) return;
  stopping = true;
  vite.kill();
  tunnel.kill();
  process.exitCode = code;
}

// cloudflared logs everything to stderr; show only what needs attention.
tunnel.stderr.setEncoding('utf8');
let shown = false;
tunnel.stderr.on('data', (text) => {
  for (const line of text.split('\n')) {
    if (/Registered tunnel connection/.test(line) && !shown) {
      shown = true;
      console.log(`\n  tunnel up: ${SITE}\n`);
    } else if (/\b(ERR|error)\b/i.test(line)) console.error(`cloudflared: ${line.trim()}`);
  }
});
tunnel.on('error', (e) => {
  console.error(e.code === 'ENOENT' ? 'cloudflared is not installed: see the setup in scripts/dev-tunnel.mjs' : `cloudflared: ${e.message}`);
  stop(1);
});
tunnel.on('exit', (code) => {
  if (stopping) return;
  console.error(`cloudflared stopped (exit ${code}). If this machine has not been set up for the ${TUNNEL} tunnel, ` +
    'see the setup in scripts/dev-tunnel.mjs.');
  stop(code || 1);
});
vite.on('exit', (code) => {
  if (!stopping) stop(code ?? 0);
});
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => stop(0));
