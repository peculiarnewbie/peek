// `/pair?host=…#code=…`: the link `peek pair` shows. Pairs straight away,
// then shows the list.
import { createFileRoute } from '@tanstack/solid-router';
import { lazy } from 'solid-js';

const Pair = lazy(() => import('../../app').then(m => ({ default: m.PairPage })));

export const Route = createFileRoute('/_app/pair')({
  ssr: false,
  head: () => ({ meta: [{ title: 'Pairing · peek' }, { name: 'robots', content: 'noindex' }] }),
  component: () => <Pair />,
});
