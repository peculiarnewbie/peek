import { SCREEN_PLUGIN } from './plugin-mode';
import { render } from 'solid-js/web';
import { setSleep } from './actions';
import { App } from './App';
import { connect } from './connection';
import { pairFromUrl } from './components/Devices';
import { HOSTED } from './hosted';
import './input';
import './touch';
import { focusScreen, layout } from './screen';
import { cfg, setFullscreen, setMessage } from './state';
import './styles.css';

render(() => <App />, document.getElementById('root')!);

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

// Installable as an app (only over HTTPS, where service workers are allowed).
if (!SCREEN_PLUGIN && 'serviceWorker' in navigator && window.isSecureContext) navigator.serviceWorker.register('/sw.js').catch(() => {});

layout();
setMessage('Connecting…');
// Opened in the background (a restored tab, a second monitor): start paused.
if (cfg.autoSleep && !document.hasFocus()) setSleep(true, true);
// The hosted client connects to whichever machine its page shows (Devices.tsx).
if (HOSTED) { if (location.pathname === '/pair') pairFromUrl(); } else connect();
focusScreen();
