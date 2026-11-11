import { createEffect } from 'solid-js';
import { BlockedNote, Message, SleepCard } from './components/Overlays';
import { IconSprite } from './components/Icons';
import { Settings } from './components/Settings';
import { Stats } from './components/Stats';
import { Toolbar } from './components/Toolbar';
import { cv } from './screen';
import { isTouch, sleeping } from './state';

export function App() {
  // Greyed-out screen behind the pause card.
  createEffect(() => document.body.classList.toggle('sleeping', sleeping()));
  // Touch layout: tap-to-open toolbar, drawn cursor, stats panel never folded.
  createEffect(() => document.body.classList.toggle('touch', isTouch()));
  return (
    <>
      <IconSprite />
      <div id="stage">{cv}</div>
      <SleepCard />
      <Message />
      <Stats />
      <Toolbar />
      <Settings />
      <BlockedNote />
    </>
  );
}
