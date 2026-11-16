import { SCREEN_PLUGIN } from './plugin-mode';
import { createEffect } from 'solid-js';
import { BlockedNote, Message, PairPrompt, SleepCard } from './components/Overlays';
import { IconSprite } from './components/Icons';
import { Keyboard } from './components/Keyboard';
import { Frame, ProviderPage } from './components/Frame';
import { HOSTED } from './hosted';
import { Settings } from './components/Settings';
import { Stats } from './components/Stats';
import { Toolbar } from './components/Toolbar';
import { stage } from './screen';
import { isTouch, live, sleeping } from './state';
import { own } from './views';
import { Show } from 'solid-js';

export function App() {
  // Greyed-out screen behind the pause card.
  createEffect(sleeping, v => { document.body.classList.toggle('sleeping', v); });
  // The last frame, dimmed while it is no longer live: what it shows may be gone.
  createEffect(live, v => { document.body.classList.toggle('stale', !v); });
  // Touch layout: tap-to-open toolbar, drawn cursor, stats panel never folded.
  createEffect(isTouch, v => { document.body.classList.toggle('touch', v); });
  // The machine's name on the tab of its own page. (The hosted site's
  // router titles its pages: site/routes/_app/screen/$.tsx.)
  if (!HOSTED) createEffect(() => own()?.name, n => { if (n) document.title = `${n} · peek`; });
  return (
    <>
      <IconSprite />
      {stage}
      <SleepCard />
      <Message />
      <Stats />
      <Show when={!SCREEN_PLUGIN}><Frame /><ProviderPage /></Show>
      <Toolbar />
      <Settings />
      <BlockedNote />
      <Keyboard />
      <Show when={!HOSTED && !SCREEN_PLUGIN}><PairPrompt /></Show>
    </>
  );
}
