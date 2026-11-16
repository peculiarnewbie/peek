// `/screen/<machine>[/<provider>]`: a paired machine's screen, or one of its
// providers' pages. Only in the browser.
import { createFileRoute } from '@tanstack/solid-router';
import { lazy } from 'solid-js';

const Screen = lazy(() => import('../../../app').then(m => ({ default: m.ScreenPage })));

export const Route = createFileRoute('/_app/screen/$')({
  ssr: false,
  head: () => ({ meta: [{ title: 'peek' }, { name: 'robots', content: 'noindex' }] }),
  component: () => <Screen />,
});
