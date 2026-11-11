import { Show } from 'solid-js';
import { setSleep, setStats, showSettings, toggleFullscreen, togglePin } from '../actions';
import { focusScreen } from '../screen';
import {
  cfg, fullscreen, live, monitors, pinned, settingsOpen, sleeping, statsMode, viewOnly,
} from '../state';
import { Icon } from './Icons';

/** The pill at the top: a dot until hovered, unless pinned or settings are open. */
export function Toolbar() {
  const status = () => {
    const m = monitors()[cfg.mon];
    const state = !live() ? 'Connecting' : sleeping() ? 'Asleep' : 'Live';
    return state + (live() && m ? ` · ${m.w}×${m.h}` : '') + (viewOnly() ? ' · view only' : '');
  };
  return (
    <div id="bar" class="panel" classList={{ open: pinned() || settingsOpen() }}>
      <div id="status">
        <span id="dot" class={!live() ? '' : sleeping() ? 'asleep' : 'on'} />
        <span id="status-text">{status()}</span>
      </div>
      <div class="more">
        <span class="sep" />
        <button id="b-sleep" class="tb" tabIndex={-1} onClick={() => setSleep(!sleeping())}>
          <Show when={sleeping()} fallback={<><Icon name="moon" /><span>Sleep</span></>}>
            <Icon name="play" /><span>Resume</span>
          </Show>
        </button>
        <button id="b-settings" class="tb" tabIndex={-1} aria-pressed={settingsOpen()}
          onClick={() => showSettings(!settingsOpen())}>
          <Icon name="sliders" /><span>Settings</span>
        </button>
        <button id="b-stats" class="tb" tabIndex={-1} aria-pressed={statsMode() !== 'off'}
          onClick={() => { setStats(statsMode() === 'off' ? 'open' : 'off'); focusScreen(); }}>
          <Icon name="chart" /><span>Stats</span>
        </button>
        <button id="b-full" class="tb icon" tabIndex={-1} title={fullscreen() ? 'Exit fullscreen' : 'Fullscreen'}
          onClick={toggleFullscreen}>
          <Icon name={fullscreen() ? 'shrink' : 'expand'} />
        </button>
        <button id="b-pin" class="tb icon" tabIndex={-1} aria-pressed={pinned()} title={pinned() ? 'Unpin' : 'Pin'}
          onClick={togglePin}>
          <Icon name="pin" />
        </button>
      </div>
    </div>
  );
}
