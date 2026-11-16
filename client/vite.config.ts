import { readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig, type Plugin } from 'vite';
import solid from '@solidjs/vite-plugin';
import { viteSingleFile } from 'vite-plugin-singlefile';

// Three builds of the same client:
// - default: one self-contained index.html (from machine/), which the host
//   embeds with include_str!;
// - `--mode screen-plugin`: the same for the opt-in screen plugin;
// - `--mode hosted`: the hosted site, a TanStack Start app (site/) that
//   `cf deploy --mode hosted` builds and deploys as described in
//   cloudflare.config.ts. Its home, docs and changelog are rendered when it is
//   built; a machine's page, pairing and /plugins only in the browser. Start
//   and the Cloudflare plugin are loaded only then, so the host's build (under
//   Bun) never needs them.

// Headers for the hosted site on Cloudflare (Workers static assets read
// `_headers`). Sockets and requests may only go to Tailscale HTTPS names.
const HOSTED_HEADERS = `/*
  Content-Security-Policy: default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self' https://*.ts.net wss://*.ts.net; frame-src https://*.ts.net; manifest-src 'self'; worker-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'
  Referrer-Policy: no-referrer
  X-Content-Type-Options: nosniff
  Permissions-Policy: camera=(self), microphone=(), geolocation=()
/assets/*
  Cache-Control: public, max-age=31536000, immutable
/index.html
  Cache-Control: no-cache
/sw.js
  Cache-Control: no-cache
/docs/*
  Cache-Control: no-cache
/changelog/*
  Cache-Control: no-cache
`;

// Files only the hosted site has, beside what it builds: its headers, and
// those in hosted/ (robots.txt, llms.txt, sitemap.xml, the preview banner og.png;
// og.svg is the banner's source). Written with the browser's files, not the Worker's.
const hostedFiles = (): Plugin => ({
  name: 'peek-hosted-files',
  generateBundle() {
    if (this.environment.name !== 'client') return;
    this.emitFile({ type: 'asset', fileName: '_headers', source: HOSTED_HEADERS });
    for (const file of readdirSync('hosted', { withFileTypes: true })) {
      if (!file.isFile() || file.name === 'og.svg') continue;
      this.emitFile({ type: 'asset', fileName: file.name, source: readFileSync(`hosted/${file.name}`) });
    }
  },
});

// The home page as built loads none of the app until it is needed: its
// script and preloads give way to site/home-loader.js, which loads them at
// once in a browser with machines paired, and in any other only when someone
// starts adding a machine. Written over the page prerendering has just saved.
let clientOutDir = '';
const clientDir = (): Plugin => ({
  name: 'peek-client-dir',
  configResolved(config) {
    clientOutDir = resolve(config.root, config.environments.client?.build.outDir ?? config.build.outDir);
  },
});
function loadAppWhenNeeded(html: string) {
  const entry = /<script type="module" async src="([^"]+)"><\/script>/.exec(html);
  if (!entry) throw new Error('home page: the app\'s script is not where it was expected');
  const preload = [...html.matchAll(/<link rel="modulepreload" href="([^"]+)"[^>]*>/g)].map(m => m[1]);
  const loader = readFileSync('site/home-loader.js', 'utf8').replace(/^\s*\/\/.*\n/gm, '')
    .replace("'%APP%'", JSON.stringify(entry[1])).replace("'%PRELOAD%'", JSON.stringify(preload));
  writeFileSync(resolve(clientOutDir, 'index.html'),
    html.replace(/<link rel="modulepreload"[^>]*>/g, '').replace(entry[0], `<script>${loader}</script>`));
}

async function hostedPlugins() {
  const { tanstackStart } = await import('@tanstack/solid-start/plugin/vite');
  const { cloudflare } = await import('@cloudflare/vite-plugin');
  return [
    cloudflare({ viteEnvironment: { name: 'ssr' } }),
    tanstackStart({
      srcDirectory: 'site',
      pages: [
        { path: '/', prerender: { onSuccess: ({ html }) => loadAppWhenNeeded(html) } },
        { path: '/docs' }, { path: '/changelog' },
      ],
      prerender: { enabled: true, crawlLinks: false, autoSubfolderIndex: true, failOnError: true },
    }),
    solid({ ssr: true }),
    hostedFiles(),
    clientDir(),
  ];
}

export default defineConfig(async ({ mode }) => {
  // Prerendering starts a preview server of its own, without --mode: it is
  // told it belongs to the hosted build by the environment.
  const hosted = mode === 'hosted' || process.env.PEEK_HOSTED_BUILD === '1';
  if (hosted) process.env.PEEK_HOSTED_BUILD = '1';
  if (hosted) return { plugins: await hostedPlugins(), build: { target: 'es2022' } };
  return {
    plugins: [solid(), viteSingleFile()],
    // From machine/index.html, not one here: the site's dev server would serve
    // that at `/`, in place of the site's home.
    root: 'machine',
    publicDir: '../public',
    // (Its `/src/main.tsx` is the client's src/, beside machine/.)
    resolve: { alias: [{ find: /^\/src\//, replacement: fileURLToPath(new URL('./src/', import.meta.url)) }] },
    build: { target: 'es2022', outDir: mode === 'screen-plugin' ? '../dist-screen' : '../dist', emptyOutDir: true },
    // `bun run dev` serves the client with hot reload and forwards the socket
    // to a host running on this machine.
    server: { proxy: { '/ws': { target: 'ws://127.0.0.1:9182', ws: true } } },
  };
});
