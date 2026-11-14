// The hosted client on Cloudflare: static files only, no Worker code.
// Browsers connect from the page straight to each machine over Tailscale,
// so nothing about the machines passes through Cloudflare.
//
// Deploy (Node, not Bun: cf cannot load this file under Bun):
//   cd client && npx cf deploy --mode hosted
// The Vite build for --mode hosted supplies the files, _headers included.
//
// With PEEK_CHANNEL=next it is the nightly site instead, a Worker of its own
// on next.peek.peculiarnewbie.com (scripts/nightly.mjs deploys it).
import { defineConfig } from 'cf/config';

const next = process.env.PEEK_CHANNEL === 'next';

export default defineConfig({
  worker: {
    name: next ? 'peek-next' : 'peek',
    compatibilityDate: '2026-11-01',
    // /pair and every machine's page load the client, which reads the URL itself.
    assets: { notFoundHandling: 'single-page-application' },
    domains: [next ? 'next.peek.peculiarnewbie.com' : 'peek.peculiarnewbie.com'],
    workersDev: false,
    previewUrls: false,
  },
});
