import { createEffect } from 'solid-js';
import { BlockedNote, Message, SleepCard } from './components/Overlays';
import { IconSprite } from './components/Icons';
import { Settings } from './components/Settings';
import { Stats } from './components/Stats';
import { Toolbar } from './components/Toolbar';
import { cv } from './screen';
import { sleeping } from './state';

export function App() {
  // Greyed-out screen behind the pause card.
  createEffect(() => document.body.classList.toggle('sleeping', sleeping()));
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
