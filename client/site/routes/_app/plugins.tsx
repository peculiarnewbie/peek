// `/plugins`: every paired machine's plugins side by side. Only in the browser.
import { createFileRoute } from '@tanstack/solid-router';
import { lazy } from 'solid-js';

const Plugins = lazy(() => import('../../app').then(m => ({ default: m.PluginsPage })));

export const Route = createFileRoute('/_app/plugins')({
  ssr: false,
  head: () => ({ meta: [{ title: 'Plugins · peek' }, { name: 'robots', content: 'noindex' }] }),
  component: () => <Plugins />,
});
