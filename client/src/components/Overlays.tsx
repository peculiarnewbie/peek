import { createSignal, Show } from 'solid-js';
import { setSleep } from '../actions';
import { pairError, pairHere, pairing, setPairError } from '../pair-here';
import { autoSlept, blocked, message, sleeping } from '../state';
import { Icon } from './Icons';

export function SleepCard() {
  return (
    <div id="sleep" classList={{ show: sleeping() }} onClick={() => setSleep(false)}>
      <div class="card panel">
        <Icon name="moon" />
        <div class="t">Paused</div>
        <div class="s">{autoSlept()
          ? 'Paused while this window is in the background. It resumes when you come back.'
          : 'Nothing is being sent. Click anywhere or press Space to resume.'}</div>
      </div>
    </div>
  );
}

/** Connection status, centred over the screen. */
export function Message() {
  return (
    <div id="msg" classList={{ show: !!message() }}>
      <div class="card panel"><div class="s">{message()}</div></div>
    </div>
  );
}

/** The client a machine serves itself, opened without a working token: pair it. */
export function PairPrompt() {
  const [code, setCode] = createSignal('');
  const [busy, setBusy] = createSignal(false);
  const submit = async () => {
    setBusy(true);
    setPairError('');
    const why = await pairHere(code());
    if (why) { setPairError(why); setBusy(false); }
  };
  return (
    <Show when={pairing()}>
      <div id="pairhere">
        <div class="card panel">
          <Icon name="monitor" />
          <div class="t">Pair this browser</div>
          <div class="s">
            {pairing() === 'wrong' ? 'This browser’s key no longer works here. ' : ''}
            Run <code>peek pair</code> on this machine and enter the code it shows. A browser that is already
            paired can show one too, under Settings.
          </div>
          <form class="mform-row" onSubmit={e => { e.preventDefault(); if (!busy() && code().trim()) void submit(); }}>
            <input class="field" placeholder="Code" autocomplete="one-time-code" autocapitalize="characters"
              spellcheck={false} value={code()} onInput={e => setCode(e.currentTarget.value)} />
            <button class="btn" type="submit" disabled={busy() || !code().trim()}>{busy() ? 'Pairing…' : 'Pair'}</button>
          </form>
          <Show when={pairError()}><p class="err">{pairError()}</p></Show>
        </div>
      </div>
    </Show>
  );
}

/** The host's foreground window is elevated, so our input does not reach it. */
export function BlockedNote() {
  return (
    <div id="note" class="panel" classList={{ show: blocked() }}>
      <Icon name="shield" />
      <span>The window in front on the host runs as administrator, so it ignores remote input.
        Start peek as administrator to control it.</span>
    </div>
  );
}
