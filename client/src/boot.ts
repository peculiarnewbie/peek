// What the screen needs once per page, whichever page shows it: the machine's
// own (main.tsx) or the hosted site's /screen/ route (site/).
import { setSleep } from './actions';
import './input';
import './touch';
import { focusScreen, layout } from './screen';
import { cfg, setFullscreen } from './state';

let booted = false;
export function boot() {
  if (booted) return;
  booted = true;
  addEventListener('resize', layout);
  // Buttons never take focus when pressed (keys belong to the screen, not to
  // the last button clicked). On a touch screen nothing but the text field
  // does: a tap anywhere else would close the phone's keyboard.
  document.addEventListener('pointerdown', e => {
    const target = e.target as Element;
    const field = !!target.closest?.('input, textarea') && target.id !== 'clipbuf';
    if (target.closest?.('button') || (e.pointerType === 'touch' && !field)) e.preventDefault();
  }, true);
  document.addEventListener('fullscreenchange', () => setFullscreen(!!document.fullscreenElement));
  layout();
  // Opened in the background (a restored tab, a second monitor): start paused.
  if (cfg.autoSleep && !document.hasFocus()) setSleep(true, true);
  focusScreen();
}

/** Installable as an app (only over HTTPS, where service workers are allowed). */
export function registerWorker() {
  if ('serviceWorker' in navigator && window.isSecureContext) navigator.serviceWorker.register('/sw.js').catch(() => {});
}
