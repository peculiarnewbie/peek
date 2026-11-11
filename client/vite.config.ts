import { readFileSync } from 'node:fs';
import { defineConfig, type Plugin } from 'vite';
import solid from 'vite-plugin-solid';
import { viteSingleFile } from 'vite-plugin-singlefile';

// Headers for the hosted client on Cloudflare (Workers static assets read
// `_headers`). Sockets and requests may only go to Tailscale HTTPS names.
const HOSTED_HEADERS = `/*
  Content-Security-Policy: default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self' https://*.ts.net wss://*.ts.net; manifest-src 'self'; worker-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'
  Referrer-Policy: no-referrer
  X-Content-Type-Options: nosniff
  Permissions-Policy: camera=(self), microphone=(), geolocation=()
/index.html
  Cache-Control: no-cache
/sw.js
  Cache-Control: no-cache
/docs/*
  Cache-Control: no-cache
`;

// Files only the hosted site has: its headers, and the docs at /docs/ (a
// static page; machine pages never take that path, see hosted.ts).
const hostedFiles = (): Plugin => ({
  name: 'peek-hosted-files',
  generateBundle() {
    this.emitFile({ type: 'asset', fileName: '_headers', source: HOSTED_HEADERS });
    this.emitFile({ type: 'asset', fileName: 'docs/index.html', source: readFileSync('docs/index.html', 'utf8') });
  },
});

// Two builds of the same client:
// - default: one self-contained index.html, which the host embeds with include_str!;
// - `--mode hosted`: the hosted client, which `cf deploy --mode hosted` builds and
//   deploys as described in cloudflare.config.ts. The Cloudflare plugin is loaded
//   only then, so the host's build (under Bun) never needs it.
export default defineConfig(async ({ mode }) => ({
  plugins: [
    solid(), viteSingleFile(),
    ...(mode === 'hosted' ? [hostedFiles(), (await import('@cloudflare/vite-plugin')).cloudflare()] : []),
  ],
  // (The Cloudflare plugin puts the hosted build in .cloudflare/output.)
  build: { target: 'es2022', outDir: 'dist', emptyOutDir: true },
  // `bun run dev` serves the client with hot reload and forwards the socket
  // to a host running on this machine.
  server: { proxy: { '/ws': { target: 'ws://127.0.0.1:9182', ws: true } } },
}));
