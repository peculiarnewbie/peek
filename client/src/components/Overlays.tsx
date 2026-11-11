import { setSleep } from '../actions';
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
