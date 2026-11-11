import { defineConfig, type Plugin } from 'vite';
import solid from 'vite-plugin-solid';
import { viteSingleFile } from 'vite-plugin-singlefile';

// Headers for the hosted client on Cloudflare (Workers static assets read
// `_headers`). Sockets and requests may only go to Tailscale HTTPS names.
const HOSTED_HEADERS = `/*
  Content-Security-Policy: default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self' https://*.ts.net wss://*.ts.net; manifest-src 'self'; worker-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'
  Referrer-Policy: no-referrer
  X-Content-Type-Options: nosniff
  Permissions-Policy: camera=(), microphone=(), geolocation=()
/index.html
  Cache-Control: no-cache
/sw.js
  Cache-Control: no-cache
`;

const hostedHeaders = (): Plugin => ({
  name: 'peek-hosted-headers',
  generateBundle() { this.emitFile({ type: 'asset', fileName: '_headers', source: HOSTED_HEADERS }); },
});

// Two builds of the same client:
// - default: one self-contained index.html, which the host embeds with include_str!;
// - `--mode hosted`: the hosted client (dist-hosted/), deployed by hosted/wrangler.jsonc.
export default defineConfig(({ mode }) => ({
  plugins: [solid(), viteSingleFile(), ...(mode === 'hosted' ? [hostedHeaders()] : [])],
  build: { target: 'es2022', outDir: mode === 'hosted' ? 'dist-hosted' : 'dist', emptyOutDir: true },
  // `bun run dev` serves the client with hot reload and forwards the socket
  // to a host running on this machine.
  server: { proxy: { '/ws': { target: 'ws://127.0.0.1:9182', ws: true } } },
}));
