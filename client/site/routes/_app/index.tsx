// `/`: the machines paired with this browser, or, with none, what peek is
// and how to start. The page is built with the introduction and the form for
// adding a machine, and loads no code of its own until it needs it: at once
// in a browser with machines paired, for the list; in any other, only when
// someone starts adding one (site/home-loader.js, put in by vite.config.ts).
import { createFileRoute } from '@tanstack/solid-router';
import { createSignal, lazy, Loading, onSettled, Show } from 'solid-js';
import { Welcome } from '../../../src/components/Welcome';
import { pageHead, SITE } from '../../seo';

const DESCRIPTION = 'peek is a small, fast remote desktop for your own Windows and Linux machines, used from any ' +
  'browser over Tailscale and tailored for short sessions. Free and open source.';

/** What search engines are told about peek, besides the page itself. */
const SOFTWARE = {
  '@context': 'https://schema.org',
  '@type': 'SoftwareApplication',
  name: 'peek',
  description: DESCRIPTION,
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

const Machines = lazy(() => import('../../app').then(m => ({ default: m.Home })));

export const Route = createFileRoute('/_app/')({
  head: () => ({
    ...pageHead({
      path: '/', title: 'peek: a small, fast remote desktop', description: DESCRIPTION,
      preview: 'A small, fast remote desktop, tailored for short sessions.',
    }),
    scripts: [{ type: 'application/ld+json', children: JSON.stringify(SOFTWARE) }],
  }),
  component: HomePage,
});

function HomePage() {
  // Built, and first drawn, as the welcome; a browser with machines paired
  // then shows its list (hidden until then by the `paired` class, _app.tsx).
  const [paired, setPaired] = createSignal(false);
  onSettled(() => {
    try { if ((localStorage.getItem('peek-machines') ?? '[]').length > 2) setPaired(true); } catch {}
  });
  return (
    <Show when={paired()} fallback={<Welcome />}>
      <Loading fallback={<Welcome />}><Machines /></Loading>
    </Show>
  );
}
