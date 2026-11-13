import { SCREEN_PLUGIN } from './plugin-mode';
import { createEffect } from 'solid-js';
import { BlockedNote, Message, SleepCard } from './components/Overlays';
import { IconSprite } from './components/Icons';
import { Keyboard } from './components/Keyboard';
import { Devices } from './components/Devices';
import { Frame, ProviderPage } from './components/Frame';
import { HOSTED } from './hosted';
import { Settings } from './components/Settings';
import { Stats } from './components/Stats';
import { Toolbar } from './components/Toolbar';
import { stage } from './screen';
import { isTouch, sleeping } from './state';
import { Show } from 'solid-js';

export function App() {
  // Greyed-out screen behind the pause card.
  createEffect(() => document.body.classList.toggle('sleeping', sleeping()));
  // Touch layout: tap-to-open toolbar, drawn cursor, stats panel never folded.
  createEffect(() => document.body.classList.toggle('touch', isTouch()));
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
      <Show when={HOSTED}><Devices /></Show>
      <BlockedNote />
      <Keyboard />
    </>
  );
}
