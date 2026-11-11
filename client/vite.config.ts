import { defineConfig } from 'vite';
import solid from 'vite-plugin-solid';
import { viteSingleFile } from 'vite-plugin-singlefile';

// One self-contained index.html, which the host embeds with include_str!.
export default defineConfig({
  plugins: [solid(), viteSingleFile()],
  build: { target: 'es2022', outDir: 'dist', emptyOutDir: true },
  // `bun run dev` serves the client with hot reload and forwards the socket
  // to a host running on this machine.
  server: { proxy: { '/ws': { target: 'ws://127.0.0.1:9182', ws: true } } },
});
