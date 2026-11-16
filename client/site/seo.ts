// What link previews and search engines are told about each page.
export const SITE = 'https://peek.peculiarnewbie.com';

/** Title, description, link preview and canonical address of a page. */
export function pageHead(o: { path: string; title: string; description: string; preview?: string }) {
  const url = SITE + o.path;
  return {
    meta: [
      { title: o.title },
      { name: 'description', content: o.description },
      { property: 'og:type', content: 'website' },
      { property: 'og:site_name', content: 'peek' },
      { property: 'og:title', content: o.title },
      { property: 'og:description', content: o.preview ?? o.description },
      { property: 'og:url', content: url },
      { property: 'og:image', content: `${SITE}/og.png` },
      { property: 'og:image:width', content: '1200' },
      { property: 'og:image:height', content: '630' },
      { property: 'og:image:alt', content: 'peek: a small, fast remote desktop, tailored for short sessions' },
      { name: 'twitter:card', content: 'summary_large_image' },
    ],
    links: [{ rel: 'canonical', href: url }],
  };
}
