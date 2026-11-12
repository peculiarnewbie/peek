// Draws what a provider asked to be shown (views.ts), from stock pieces.
// Nothing here knows what any provider is for.
import { For, Index, Match, Show, Switch } from 'solid-js';
import type { ButtonItem, Item } from '../views';

/** A value with its unit set smaller, for the stat tiles. */
export function Figure(props: { text: string }) {
  const parts = () => props.text.split(' ');
  return <>{parts()[0]}<small>{parts().slice(1).join(' ')}</small></>;
}

export function ItemView(props: { item: Item; disabled?: boolean; onPress: (item: ButtonItem) => void }) {
  return (
    <Switch>
      <Match when={props.item.kind === 'text' && props.item}>{i => (
        <p class="vtext" classList={{ quiet: i().tone === 'hint', wrong: i().tone === 'error', busy: !!i().busy }}>
          {i().text}
        </p>
      )}</Match>
      <Match when={props.item.kind === 'button' && props.item}>{i => (
        <div class="vrow">
          <button class="btn small" classList={{ alt: i().style !== 'primary' }} tabIndex={-1} disabled={props.disabled}
            onClick={() => props.onPress(i())}>{i().label}</button>
          <Show when={i().note}><span class="vtext quiet">{i().note}</span></Show>
        </div>
      )}</Match>
      <Match when={props.item.kind === 'tiles' && props.item}>{i => (
        <div class="tiles">
          <For each={i().tiles}>{t => (
            <div class="tile"><div class="k">{t.label}</div><div class="v"><Figure text={String(t.value)} /></div></div>
          )}</For>
        </div>
      )}</Match>
      <Match when={props.item.kind === 'meters' && props.item}>{i => (
        <div class="meters">
          <For each={i().meters}>{m => <>
            <span>{m.label}</span>
            <div class="meter"><div style={{ width: `${Math.min(Math.max(Number(m.fraction) || 0, 0), 1) * 100}%` }} /></div>
            <span>{m.text}</span>
          </>}</For>
        </div>
      )}</Match>
    </Switch>
  );
}

export function Items(props: { items: Item[]; disabled?: boolean; onPress: (item: ButtonItem) => void }) {
  return <Index each={props.items}>{item => <ItemView item={item()} disabled={props.disabled} onPress={props.onPress} />}</Index>;
}
