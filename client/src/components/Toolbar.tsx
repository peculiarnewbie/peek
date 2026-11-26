import { createEffect, createSignal, For, onSettled, Show } from 'solid-js';
import { setSleep, setStats, showSettings, toggleFullscreen, toggleKeyboard, togglePin, toggleTouchMode } from '../actions';
import { focusScreen } from '../screen';
import {
  barOpen, cfg, fullscreen, isTouch, kbdOpen, live, monitors, pinned, setBarOpen, setIsTouch, settingsOpen, sleeping,
  statsMode, touchMode, viewOnly,
} from '../state';
import { current, home, HOSTED } from '../hosted';
import { Icon } from './Icons';
import { appPages, framed, openPage } from './Frame';

/** The pill at the top: a dot until hovered (or tapped), unless pinned or settings are open. */
export function Toolbar() {
  const [pluginsOpen, setPluginsOpen] = createSignal(false);
  const open = () => pinned() || settingsOpen() || barOpen() || pluginsOpen();
  // No hover on a touch screen: the first tap opens the bar, a tap on the screen
  // closes it. Opened on the click itself (in the capture phase, stopped there):
  // opened any sooner, the bar grows under the finger and the click lands on
  // whichever button is there now.
  let bar!: HTMLDivElement;
  let pluginsMenu: HTMLDivElement | undefined;
  onSettled(() => {
    bar.addEventListener('click', e => {
      if ((e as PointerEvent).pointerType !== 'touch') return;
      setIsTouch(true);
      if (!open()) { setBarOpen(true); e.stopPropagation(); }
    }, true);
    const onPointerDown = (e: PointerEvent) => {
      if (pluginsOpen() && pluginsMenu && !pluginsMenu.contains(e.target as Node)) setPluginsOpen(false);
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && pluginsOpen()) setPluginsOpen(false);
    };
    document.addEventListener('pointerdown', onPointerDown, true);
    document.addEventListener('keydown', onKeyDown, true);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown, true);
      document.removeEventListener('keydown', onKeyDown, true);
    };
  });
  createEffect(() => settingsOpen() || appPages().length <= 2, close => {
    if (close) setPluginsOpen(false);
  });
  const status = () => {
    const m = monitors()[cfg.mon];
    const state = !live() ? 'Connecting' : sleeping() ? 'Paused' : 'Live';
    return state + (live() && m ? ` · ${m.w}×${m.h}` : '') + (viewOnly() ? ' · view only' : '');
  };
  const pluginActive = () => {
    const p = framed();
    return !!p && appPages().includes(p);
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
        <Show when={appPages().length > 2} fallback={
          <For each={appPages()}>{p =>
            <button class="tb text" tabindex={-1} title={`${p}: its page on this machine`} aria-pressed={framed() === p ? 'true' : 'false'}
              onClick={() => openPage(p)}><span>{p}</span></button>
          }</For>
        }>
          <div class="tb-menu" ref={pluginsMenu}>
            <button id="b-plugins" class="tb text" tabindex={-1} title="Plugins with pages on this machine"
              aria-haspopup="menu" aria-expanded={pluginsOpen() ? 'true' : 'false'}
              aria-pressed={pluginsOpen() || pluginActive() ? 'true' : 'false'}
              onClick={() => setPluginsOpen(!pluginsOpen())}>
              <span>Plugins</span><Icon name="chev" />
            </button>
            <Show when={pluginsOpen()}>
              <div class="tb-drop panel" role="menu" aria-label="Plugins">
                <For each={appPages()}>{p =>
                  <button role="menuitem" tabindex={-1} title={`${p}: its page on this machine`}
                    aria-pressed={framed() === p ? 'true' : 'false'}
                    onClick={() => { setPluginsOpen(false); openPage(p); }}>{p}</button>
                }</For>
              </div>
            </Show>
          </div>
        </Show>
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
        <Show when={!isTouch()}>
          <button id="b-kbd" class="tb" tabindex={-1} aria-label="Keyboard" title="On-screen keyboard"
            aria-pressed={kbdOpen() ? 'true' : 'false'} onClick={toggleKeyboard}>
            <Icon name="keyboard" /><span>Keyboard</span>
          </button>
        </Show>
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
