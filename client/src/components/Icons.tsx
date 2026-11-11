// Line icons, drawn once in a sprite and referenced by id.
export type IconName =
  | 'moon' | 'play' | 'sliders' | 'chart' | 'expand' | 'shrink' | 'pin' | 'x' | 'chev' | 'shield';

export function IconSprite() {
  return (
    <svg width="0" height="0" style={{ position: 'absolute' }}>
      <symbol id="i-moon" viewBox="0 0 24 24"><path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z" /></symbol>
      <symbol id="i-play" viewBox="0 0 24 24"><path d="M8 5.5v13l10-6.5z" /></symbol>
      <symbol id="i-sliders" viewBox="0 0 24 24"><path d="M4 7h9M17 7h3M4 17h3M11 17h9" /><circle cx="15" cy="7" r="2" /><circle cx="9" cy="17" r="2" /></symbol>
      <symbol id="i-chart" viewBox="0 0 24 24"><path d="M5 20v-7M12 20V5M19 20v-10" /></symbol>
      <symbol id="i-expand" viewBox="0 0 24 24"><path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" /></symbol>
      <symbol id="i-shrink" viewBox="0 0 24 24"><path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5" /></symbol>
      <symbol id="i-pin" viewBox="0 0 24 24"><path d="M9.5 4h5l-1 5.5 3 3h-9l3-3zM12 12.5V20" /></symbol>
      <symbol id="i-x" viewBox="0 0 24 24"><path d="M6.5 6.5l11 11M17.5 6.5l-11 11" /></symbol>
      <symbol id="i-chev" viewBox="0 0 24 24"><path d="M6.5 9.5l5.5 5.5 5.5-5.5" /></symbol>
      <symbol id="i-shield" viewBox="0 0 24 24"><path d="M12 3l7 3v5c0 4.5-3 8-7 10-4-2-7-5.5-7-10V6zM12 8v5M12 16h.01" /></symbol>
    </svg>
  );
}

export function Icon(props: { name: IconName }) {
  return <svg class="i"><use href={`#i-${props.name}`} /></svg>;
}
