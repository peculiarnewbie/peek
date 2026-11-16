// Hosted site: a machine's page, at `/screen/<name>[/<provider>]` (site/).
// The page decides the connection: it connects to the machine its address
// names, and lets go when it changes or the page closes.
import { createEffect, createMemo, onSettled, untrack } from 'solid-js';
import { App } from '../App';
import { showSettings } from '../actions';
import { boot } from '../boot';
import { connect, disconnect } from '../connection';
import { about, current, learn, navigate } from '../hosted';
import { focusScreen } from '../screen';
import { setMessage } from '../state';

export function HostedScreen() {
  // (The connection code reads the state it needs as it goes: untracked here,
  // where only the machine shown decides when to run.)
  onSettled(() => {
    untrack(boot);
    return () => untrack(() => { disconnect(); showSettings(false); setMessage(''); });
  });
  // (A memo: the list of machines changes whenever one says something new
  // of itself, and that must not reconnect the one being shown.)
  const shownId = createMemo(() => current()?.id);
  createEffect(shownId, id => untrack(() => {
    disconnect();
    if (!id) {
      // Not (or no longer) a paired machine: back to the list.
      setMessage('');
      navigate('/', true);
      return;
    }
    const m = current()!;
    setMessage(`Connecting to ${m.name}…`);
    connect();
    // Its providers as they are now: this page may be the first to ask.
    about(m.host).then(a => { if (a.id === m.id) learn(m, a); }, () => {});
    focusScreen();
  }));
  return <App />;
}
