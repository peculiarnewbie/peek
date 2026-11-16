// The document every page of the site is in. The app's pages add its styles
// and icons (_app.tsx); the docs and changelog have their own.
import { createRootRoute, HeadContent, Scripts } from '@tanstack/solid-router';
import { Loading } from 'solid-js';
import type { JSX } from '@solidjs/web';
import { NotFound } from '../NotFound';

export const Route = createRootRoute({
  head: () => ({
    meta: [
      { charSet: 'utf-8' },
      { name: 'viewport', content: 'width=device-width,initial-scale=1' },
      { name: 'theme-color', content: '#0d0d0d' },
    ],
    links: [
      { rel: 'icon', href: '/icon.svg', type: 'image/svg+xml' },
      { rel: 'manifest', href: '/manifest.webmanifest' },
      { rel: 'apple-touch-icon', href: '/icon-192.png' },
    ],
  }),
  shellComponent: Document,
  notFoundComponent: NotFound,
});

function Document(props: { children: JSX.Element }) {
  return (
    <html lang="en">
      <head><HeadContent /></head>
      <body>
        <Loading>{props.children}</Loading>
        <Scripts />
      </body>
    </html>
  );
}
