// The client a machine serves itself (and the screen plugin's build). The
// hosted site starts from site/ instead.
import { SCREEN_PLUGIN } from './plugin-mode';
import { render } from '@solidjs/web';
import { App } from './App';
import { boot, registerWorker } from './boot';
import { connect } from './connection';
import { codeInUrl, pairFromHash } from './pair-here';
import { setMessage } from './state';
import './styles.css';

render(() => <App />, document.getElementById('root')!);
if (!SCREEN_PLUGIN) registerWorker();
setMessage('Connecting…');
boot();
if (!SCREEN_PLUGIN && codeInUrl()) pairFromHash();
else connect();
