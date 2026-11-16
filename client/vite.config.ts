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
const TITLE = 'peek: a small, fast remote desktop';
const DESCRIPTION = 'A small, fast remote desktop, tailored for short sessions.';
/** For search results: what it is, for what, and how, in a sentence or two. */
const SEARCH_DESCRIPTION = 'peek is a small, fast remote desktop for your own Windows and Linux machines, ' +
  'used from any browser over Tailscale and tailored for short sessions. Free and open source.';
const PREVIEW: [string, string, string][] = [
  ['name', 'description', SEARCH_DESCRIPTION],
  ['property', 'og:type', 'website'],
  ['property', 'og:site_name', 'peek'],
  ['property', 'og:title', TITLE],
  ['property', 'og:description', DESCRIPTION],
  ['property', 'og:url', `${SITE}/`],
  ['property', 'og:image', `${SITE}/og.png`],
  ['property', 'og:image:width', '1200'],
  ['property', 'og:image:height', '630'],
  ['property', 'og:image:alt', 'peek: a small, fast remote desktop, tailored for short sessions'],
  ['name', 'twitter:card', 'summary_large_image'],
];

/** What search engines are told about peek, besides the page itself. */
const SOFTWARE = {
  '@context': 'https://schema.org',
  '@type': 'SoftwareApplication',
  name: 'peek',
  description: SEARCH_DESCRIPTION,
  url: `${SITE}/`,
  applicationCategory: 'UtilitiesApplication',
  applicationSubCategory: 'Remote desktop',
  operatingSystem: 'Windows, Linux',
  license: 'https://opensource.org/licenses/MIT',
  offers: { '@type': 'Offer', price: '0', priceCurrency: 'USD' },
  downloadUrl: 'https://www.npmjs.com/package/@peculiarnewbie/peek',
  sameAs: ['https://github.com/peculiarnewbie/peek', 'https://www.npmjs.com/package/@peculiarnewbie/peek'],
  image: `${SITE}/og.png`,
};

/** The home page as plain HTML, for anything that reads the page without
 *  running it (see hosted/home.fragment.html), and the script that takes it
 *  away again before a browser draws it. */
const homeForReaders = () => `<div id="root">${readFileSync('hosted/home.fragment.html', 'utf8')
  .replace(/<!--[\s\S]*?-->\s*/g, '')}</div>\n<script>document.getElementById('about').remove()</script>`;

// What only the hosted site has: its headers, title, tags for link previews
// and search engines, the home page for readers that do not run it, and the
// files in hosted/ (the docs at /docs/, the changelog at /changelog/,
// robots.txt, sitemap.xml, the preview banner og.png; og.svg is the banner's
// source). Machine pages are under /screen/, clear of these: see hosted.ts.
const hostedFiles = (): Plugin => ({
  name: 'peek-hosted-files',
  transformIndexHtml: html => ({
    html: html.replace('<title>peek</title>', `<title>${TITLE}</title>`)
      .replace('<div id="root"></div>', homeForReaders()),
    tags: [
      ...PREVIEW.map(([key, name, content]) => ({ tag: 'meta', attrs: { [key]: name, content }, injectTo: 'head' as const })),
      { tag: 'link', attrs: { rel: 'canonical', href: `${SITE}/` }, injectTo: 'head' as const },
      { tag: 'script', attrs: { type: 'application/ld+json' }, children: JSON.stringify(SOFTWARE), injectTo: 'head' as const },
    ],
  }),
  generateBundle() {
    this.emitFile({ type: 'asset', fileName: '_headers', source: HOSTED_HEADERS });
    for (const file of readdirSync('hosted', { recursive: true, withFileTypes: true })) {
      if (!file.isFile() || file.name === 'og.svg' || file.name.endsWith('.fragment.html')) continue;
      const path = `${file.parentPath}/${file.name}`.replace(/\\/g, '/');
      // HTML as text: later plugins rewrite emitted HTML and expect a string.
      const source = path.endsWith('.html') ? readFileSync(path, 'utf8') : readFileSync(path);
      this.emitFile({ type: 'asset', fileName: path.slice('hosted/'.length), source });
    }
  },
});

// The app's script after the page's text, not before it in <head>: some
// readers only look at a page's first so many kilobytes, and the script is
// most of them. (A module script runs after the page is read either way.)
const scriptLast = (): Plugin => ({
  name: 'peek-script-last',
  enforce: 'post',
  generateBundle(_, bundle) {
    const page = bundle['index.html'];
    if (page?.type !== 'asset' || typeof page.source !== 'string') return;
    const html = page.source;
    const start = html.indexOf('<script type="module"');
    const end = html.indexOf('</script>', start) + '</script>'.length;
    if (start < 0 || start > html.indexOf('<body') || !html.includes('</body>')) return;
    const script = html.slice(start, end);
    page.source = (html.slice(0, start) + html.slice(end)).replace('</body>', `${script}\n</body>`);
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
    ...(mode === 'hosted' ? [hostedFiles(), scriptLast(), (await import('@cloudflare/vite-plugin')).cloudflare()] : []),
  ],
  // (The Cloudflare plugin puts the hosted build in .cloudflare/output.)
  build: { target: 'es2022', outDir: mode === 'screen-plugin' ? 'dist-screen' : 'dist', emptyOutDir: true },
  // `bun run dev` serves the client with hot reload and forwards the socket
  // to a host running on this machine.
  server: { proxy: { '/ws': { target: 'ws://127.0.0.1:9182', ws: true } } },
}));
