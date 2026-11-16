// The hosted site on Cloudflare: a TanStack Start app (site/). Its home, docs
// and changelog are files, built ahead; the Worker only answers what is not a
// file, which is a machine's page, pairing and /plugins: the page they load
// in, with nothing in it but the app's code. Browsers connect from the page
// straight to each machine over Tailscale, so nothing about the machines
// passes through Cloudflare.
//
// Deploy (Node, not Bun: cf cannot load this file under Bun):
//   cd client && npx vite build --mode hosted && npx cf deploy --prebuilt
// (`bun run deploy:hosted` does both.) The Vite build writes what is deployed,
// _headers included.
import { defineConfig } from 'cf/config';

export default defineConfig({
  worker: {
    name: 'peek',
    compatibilityDate: '2026-11-01',
    compatibilityFlags: ['nodejs_compat'],
    entrypoint: '@tanstack/solid-start/server-entry',
    domains: ['peek.peculiarnewbie.com'],
    workersDev: false,
    previewUrls: false,
  },
});
