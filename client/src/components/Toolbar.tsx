import { For, onSettled, Show } from 'solid-js';
import { setSleep, setStats, showSettings, toggleFullscreen, togglePin, toggleTouchMode } from '../actions';
import { focusScreen } from '../screen';
import {
  barOpen, cfg, fullscreen, isTouch, live, monitors, pinned, setBarOpen, setIsTouch, settingsOpen, sleeping,
  statsMode, touchMode, viewOnly,
} from '../state';
import { current, home, HOSTED } from '../hosted';
import { Icon } from './Icons';
import { appPages, framed, openPage } from './Frame';

/** The pill at the top: a dot until hovered (or tapped), unless pinned or settings are open. */
export function Toolbar() {
  const open = () => pinned() || settingsOpen() || barOpen();
  // No hover on a touch screen: the first tap opens the bar, a tap on the screen
  // closes it. Opened on the click itself (in the capture phase, stopped there):
  // opened any sooner, the bar grows under the finger and the click lands on
  // whichever button is there now.
  let bar!: HTMLDivElement;
  onSettled(() => {
    bar.addEventListener('click', e => {
      if ((e as PointerEvent).pointerType !== 'touch') return;
      setIsTouch(true);
      if (!open()) { setBarOpen(true); e.stopPropagation(); }
    }, true);
  });
  const status = () => {
    const m = monitors()[cfg.mon];
    const state = !live() ? 'Connecting' : sleeping() ? 'Paused' : 'Live';
    return state + (live() && m ? ` · ${m.w}×${m.h}` : '') + (viewOnly() ? ' · view only' : '');
  };
  return (
    <div id="bar" class={['panel', { open: open() }]} ref={bar}>
      <div id="status">
        <span id="dot" class={!live() ? '' : sleeping() ? 'asleep' : 'on'} />
        <span id="status-text">{status()}</span>
      </div>
      <div class="more">
        <span class="sep" />
        <Show when={HOSTED}>
          <button id="b-machines" class="tb" tabindex={-1} title="All machines" aria-label={`${current()?.name}: all machines`} onClick={home}>
            <Icon name="back" /><span>{current()?.name}</span>
          </button>
        </Show>
        {/* The machine's apps with a page of their own (Frame.tsx); plugins are under Settings. */}
        <For each={appPages()}>{p =>
          <button class="tb text" tabindex={-1} title={`${p}: its page on this machine`} aria-pressed={framed() === p ? 'true' : 'false'}
            onClick={() => openPage(p)}><span>{p}</span></button>
        }</For>
        {/* Labels as well as text: a narrow screen shows the icons only. */}
        <button id="b-sleep" class="tb" tabindex={-1} onClick={() => setSleep(!sleeping())}
          aria-label={sleeping() ? 'Resume' : 'Pause'}
          title={sleeping() ? 'Resume the picture' : 'Pause the picture in this browser, keeping the connection'}>
          <Show when={sleeping()} fallback={<><Icon name="pause" /><span>Pause</span></>}>
            <Icon name="play" /><span>Resume</span>
          </Show>
        </button>
        <button id="b-settings" class="tb" tabindex={-1} aria-label="Settings" title="Settings" aria-pressed={settingsOpen() ? 'true' : 'false'}
          onClick={() => showSettings(!settingsOpen())}>
          <Icon name="sliders" /><span>Settings</span>
        </button>
        <button id="b-stats" class="tb" tabindex={-1} aria-label="Stats" title="Stats" aria-pressed={statsMode() !== 'off' ? 'true' : 'false'}
          onClick={() => { setStats(statsMode() === 'off' ? 'open' : 'off'); focusScreen(); }}>
          <Icon name="chart" /><span>Stats</span>
        </button>
        <Show when={isTouch()}>
          <button id="b-touch" class="tb" tabindex={-1} onClick={toggleTouchMode}
            aria-label={touchMode() === 'trackpad' ? 'Trackpad' : 'Touch'}
            title={touchMode() === 'trackpad'
              ? 'Trackpad: drag to move the pointer, tap to click. Switch to touch'
              : 'Touch: tap where you want to click. Switch to trackpad'}>
            <Icon name={touchMode() === 'trackpad' ? 'trackpad' : 'hand'} />
            <span>{touchMode() === 'trackpad' ? 'Trackpad' : 'Touch'}</span>
          </button>
        </Show>
        <button id="b-full" class="tb icon" tabindex={-1} title={fullscreen() ? 'Exit fullscreen' : 'Fullscreen'}
          aria-label={fullscreen() ? 'Exit fullscreen' : 'Fullscreen'} onClick={toggleFullscreen}>
          <Icon name={fullscreen() ? 'shrink' : 'expand'} />
        </button>
        <button id="b-pin" class="tb icon" tabindex={-1} aria-pressed={pinned() ? 'true' : 'false'} title={pinned() ? 'Unpin' : 'Pin'}
          aria-label={pinned() ? 'Unpin the toolbar' : 'Pin the toolbar'} onClick={togglePin}>
          <Icon name="pin" />
        </button>
      </div>
    </div>
  );
}
