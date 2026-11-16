// `/screen/<machine>[/<provider>]`: a paired machine's screen, or one of its
// providers' pages. Only in the browser.
import { createFileRoute } from '@tanstack/solid-router';
import { lazy } from 'solid-js';

const Screen = lazy(() => import('../../../app').then(m => ({ default: m.ScreenPage })));

/** The machine's name on the tab, from this browser's list as the app keeps
 *  it (src/hosted.ts): the app's own code is not loaded yet when this runs. */
function title(splat = '') {
  try {
    const slug = splat.split('/')[0].toLowerCase();
    const list: { id: string; name: string; host: string }[] = JSON.parse(localStorage.getItem('peek-machines') || '[]');
    const m = list.find(m => m.host.split(/[.:]/)[0].toLowerCase() === slug || m.id === slug);
    if (m?.name) return `${m.name} · peek`;
  } catch {}
  return 'peek';
}

export const Route = createFileRoute('/_app/screen/$')({
  ssr: false,
  head: ({ params }) => ({ meta: [{ title: title(params._splat) }, { name: 'robots', content: 'noindex' }] }),
  component: () => <Screen />,
});
