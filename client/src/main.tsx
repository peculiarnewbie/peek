import { render } from 'solid-js/web';
import { setSleep } from './actions';
import { App } from './App';
import { connect } from './connection';
import './input';
import { cv, layout } from './screen';
import { cfg, setFullscreen, setMessage } from './state';
import './styles.css';

render(() => <App />, document.getElementById('root')!);

addEventListener('resize', layout);
document.addEventListener('fullscreenchange', () => setFullscreen(!!document.fullscreenElement));

layout();
setMessage('Connecting…');
// Opened in the background (a restored tab, a second monitor): start paused.
if (cfg.autoSleep && !document.hasFocus()) setSleep(true, true);
connect();
cv.focus();
