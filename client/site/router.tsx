import { createRouter } from '@tanstack/solid-router';
import { setSiteRouter, type SiteRouter } from '../src/site-router';
import { routeTree } from './routeTree.gen';

export function getRouter() {
  // 'preserve': /docs/ is a folder's index.html on Cloudflare, which adds the
  // slash; the router must not take it off again.
  const router = createRouter({ routeTree, scrollRestoration: true, trailingSlash: 'preserve' });
  if (typeof window !== 'undefined') setSiteRouter(router as unknown as SiteRouter);
  return router;
}

declare module '@tanstack/solid-router' {
  interface Register { router: ReturnType<typeof getRouter> }
}
