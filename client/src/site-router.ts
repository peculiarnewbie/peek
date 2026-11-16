// The hosted site's router (site/router.tsx), for the client code it loads:
// navigation in hosted.ts goes through it, so the site's routes follow. Null
// on a machine's own page, which has no router.
export interface SiteRouter {
  navigate(options: { to: string; replace?: boolean }): unknown;
  subscribe(event: 'onBeforeLoad', fn: (e: { toLocation: { pathname: string } }) => void): unknown;
}
let router: SiteRouter | null = null;
export const siteRouter = () => router;
export function setSiteRouter(r: SiteRouter) { router = r; }
