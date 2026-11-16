// An address the site does not have. Until 0.2.0 a machine's page was
// `/<name>` (or `/<id>`): bookmarks of one still lead to it, in the browser.
import { onSettled } from 'solid-js';

export function NotFound() {
  onSettled(() => {
    if (/^\/[^/]+$/.test(location.pathname)) {
      location.replace('/screen' + location.pathname + location.search + location.hash);
    }
  });
  return (
    <main style={{ 'max-width': '560px', margin: '64px auto', padding: '0 18px', color: '#c3c2b7', font: '15px/1.6 system-ui, sans-serif' }}>
      <h1 style={{ color: '#f0efec', 'font-size': '22px' }}>Nothing here</h1>
      <p>This page does not exist. <a style={{ color: '#f0efec' }} href="/">Back to peek</a>.</p>
    </main>
  );
}
