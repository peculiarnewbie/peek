// `/`: the machines paired with this browser, or, with none, what peek is
// and how to start. The site is built with the introduction; the list loads
// in the browser.
import { ClientOnly, createFileRoute } from '@tanstack/solid-router';
import { lazy, Loading } from 'solid-js';
import { Foot, Header, Intro } from '../../../src/components/Intro';
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

/** What the page is built with, and shows until the list has loaded. */
const Static = () => <main id="devices" class="static"><Header /><Intro /><Foot /></main>;

function HomePage() {
  return (
    <ClientOnly fallback={<Static />}>
      <Loading fallback={<Static />}><Machines /></Loading>
    </ClientOnly>
  );
}
