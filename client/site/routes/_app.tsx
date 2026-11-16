// The app's pages: the list of machines, every machine's plugins, pairing,
// and a machine's own page. They share the app's styles and icons. Its code
// is loaded only in the browser (it reaches for the window, the screen and
// this browser's storage as it loads), from lazy imports in each route.
import { createFileRoute, Outlet } from '@tanstack/solid-router';
import { onSettled } from 'solid-js';
import { IconSprite } from '../../src/components/Icons';
import appCss from '../../src/styles.css?url';

// A browser with machines paired sees its list once the code has loaded; the
// introduction the page was built with stays hidden until then.
const PAIRED = `try{if((localStorage.getItem('peek-machines')||'[]').length>2)document.documentElement.classList.add('paired')}catch(e){}`;

export const Route = createFileRoute('/_app')({
  head: () => ({
    meta: [{ name: 'viewport', content: 'width=device-width,initial-scale=1,maximum-scale=1,user-scalable=no,interactive-widget=resizes-content' }],
    links: [{ rel: 'stylesheet', href: appCss }],
    scripts: [{ children: PAIRED }],
  }),
  component: AppLayout,
});

function AppLayout() {
  onSettled(() => {
    if ('serviceWorker' in navigator && window.isSecureContext) navigator.serviceWorker.register('/sw.js').catch(() => {});
  });
  return <><IconSprite /><Outlet /></>;
}
