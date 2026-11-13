import { readdirSync, readFileSync } from 'node:fs';
import { defineConfig, type Plugin } from 'vite';
import solid from 'vite-plugin-solid';
import { viteSingleFile } from 'vite-plugin-singlefile';

// Headers for the hosted client on Cloudflare (Workers static assets read
// `_headers`). Sockets and requests may only go to Tailscale HTTPS names.
const HOSTED_HEADERS = `/*
  Content-Security-Policy: default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self' https://*.ts.net wss://*.ts.net; frame-src https://*.ts.net; manifest-src 'self'; worker-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'
  Referrer-Policy: no-referrer
  X-Content-Type-Options: nosniff
  Permissions-Policy: camera=(self), microphone=(), geolocation=()
/index.html
  Cache-Control: no-cache
/sw.js
  Cache-Control: no-cache
/docs/*
  Cache-Control: no-cache
/changelog/*
  Cache-Control: no-cache
`;

// Where the hosted site lives, for link previews (they need absolute URLs).
const SITE = 'https://peek.peculiarnewbie.com';
const DESCRIPTION = 'A small, fast remote desktop, tailored for short sessions.';
const PREVIEW: [string, string, string][] = [
  ['name', 'description', DESCRIPTION],
  ['property', 'og:type', 'website'],
  ['property', 'og:site_name', 'peek'],
  ['property', 'og:title', 'peek'],
  ['property', 'og:description', DESCRIPTION],
  ['property', 'og:url', `${SITE}/`],
  ['property', 'og:image', `${SITE}/og.png`],
  ['property', 'og:image:width', '1200'],
  ['property', 'og:image:height', '630'],
  ['property', 'og:image:alt', 'peek: a small, fast remote desktop, tailored for short sessions'],
  ['name', 'twitter:card', 'summary_large_image'],
];

// What only the hosted site has: its headers, link preview tags, and the
// files in hosted/ (the docs at /docs/, the changelog at /changelog/, the
// preview banner og.png; og.svg is the banner's source). Machine pages are
// under /screen/, clear of these: see hosted.ts.
const hostedFiles = (): Plugin => ({
  name: 'peek-hosted-files',
  transformIndexHtml: () => PREVIEW.map(([key, name, content]) =>
    ({ tag: 'meta', attrs: { [key]: name, content }, injectTo: 'head' as const })),
  generateBundle() {
    this.emitFile({ type: 'asset', fileName: '_headers', source: HOSTED_HEADERS });
    for (const file of readdirSync('hosted', { recursive: true, withFileTypes: true })) {
      if (!file.isFile() || file.name === 'og.svg') continue;
      const path = `${file.parentPath}/${file.name}`.replace(/\\/g, '/');
      // HTML as text: later plugins rewrite emitted HTML and expect a string.
      const source = path.endsWith('.html') ? readFileSync(path, 'utf8') : readFileSync(path);
      this.emitFile({ type: 'asset', fileName: path.slice('hosted/'.length), source });
    }
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
  build: { target: 'es2022', outDir: mode === 'screen-plugin' ? 'dist-screen' : 'dist', emptyOutDir: true },
  // `bun run dev` serves the client with hot reload and forwards the socket
  // to a host running on this machine.
  server: { proxy: { '/ws': { target: 'ws://127.0.0.1:9182', ws: true } } },
}));
