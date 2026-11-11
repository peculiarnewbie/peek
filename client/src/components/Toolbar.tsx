import { onMount, Show } from 'solid-js';
import { setSleep, setStats, showSettings, toggleFullscreen, togglePin, toggleTouchMode } from '../actions';
import { focusScreen } from '../screen';
import {
  barOpen, cfg, fullscreen, isTouch, live, monitors, pinned, setBarOpen, setIsTouch, settingsOpen, sleeping,
  statsMode, touchMode, viewOnly,
} from '../state';
import { dock, setDock } from '../keyboard';
import { Icon } from './Icons';

/** The pill at the top: a dot until hovered (or tapped), unless pinned or settings are open. */
export function Toolbar() {
  const open = () => pinned() || settingsOpen() || barOpen();
  // No hover on a touch screen: the first tap opens the bar, a tap on the screen
  // closes it. Opened on the click itself (in the capture phase, stopped there):
  // opened any sooner, the bar grows under the finger and the click lands on
  // whichever button is there now.
  let bar!: HTMLDivElement;
  onMount(() => bar.addEventListener('click', e => {
    if ((e as PointerEvent).pointerType !== 'touch') return;
    setIsTouch(true);
    if (!open()) { setBarOpen(true); e.stopPropagation(); }
  }, true));
  const status = () => {
    const m = monitors()[cfg.mon];
    const state = !live() ? 'Connecting' : sleeping() ? 'Asleep' : 'Live';
    return state + (live() && m ? ` · ${m.w}×${m.h}` : '') + (viewOnly() ? ' · view only' : '');
  };
  return (
    <div id="bar" class="panel" classList={{ open: open() }} ref={bar}>
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
        <Show when={isTouch()}>
          <button id="b-kbd" class="tb" tabIndex={-1} aria-pressed={dock() !== 'off'}
            onClick={() => setDock(dock() === 'off' ? 'text' : 'off')}>
            <Icon name="keyboard" /><span>Keyboard</span>
          </button>
          <button id="b-touch" class="tb" tabIndex={-1} onClick={toggleTouchMode}
            title={touchMode() === 'trackpad'
              ? 'Trackpad: drag to move the pointer, tap to click. Switch to touch'
              : 'Touch: tap where you want to click. Switch to trackpad'}>
            <Icon name={touchMode() === 'trackpad' ? 'trackpad' : 'hand'} />
            <span>{touchMode() === 'trackpad' ? 'Trackpad' : 'Touch'}</span>
          </button>
        </Show>
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
