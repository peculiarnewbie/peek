// Line icons, drawn once in a sprite and referenced by id.
export type IconName =
  | 'moon' | 'play' | 'sliders' | 'chart' | 'expand' | 'shrink' | 'pin' | 'x' | 'chev' | 'shield'
  | 'trackpad' | 'hand' | 'keyboard' | 'backspace' | 'enter' | 'monitor' | 'back' | 'plus' | 'qr' | 'copy' | 'check' | 'star';

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
      <symbol id="i-trackpad" viewBox="0 0 24 24"><rect x="3.5" y="5" width="17" height="14" rx="2.5" /><path d="M3.5 15h17M12 15v4" /></symbol>
      <symbol id="i-hand" viewBox="0 0 24 24"><path d="M9 12.5V5a1.5 1.5 0 0 1 3 0v6M12 10.5a1.5 1.5 0 0 1 3 0V12M15 11.5a1.5 1.5 0 0 1 3 0v3.5a6 6 0 0 1-6 6h-.6a5.5 5.5 0 0 1-4.6-2.5L4.6 15a1.5 1.5 0 0 1 2.4-1.8L9 15" /></symbol>
      <symbol id="i-keyboard" viewBox="0 0 24 24"><rect x="2.5" y="6" width="19" height="12" rx="2" /><path d="M6 10h.01M9 10h.01M12 10h.01M15 10h.01M18 10h.01M6 14h.01M18 14h.01M9 14h6" /></symbol>
      <symbol id="i-backspace" viewBox="0 0 24 24"><path d="M9 5.5h11a1.5 1.5 0 0 1 1.5 1.5v10a1.5 1.5 0 0 1-1.5 1.5H9L3 12zM11.5 9.5l5 5M16.5 9.5l-5 5" /></symbol>
      <symbol id="i-enter" viewBox="0 0 24 24"><path d="M19 5v6a3 3 0 0 1-3 3H5M9 10l-4 4 4 4" /></symbol>
      <symbol id="i-monitor" viewBox="0 0 24 24"><rect x="3" y="4.5" width="18" height="12" rx="2" /><path d="M9 20h6M12 16.5V20" /></symbol>
      <symbol id="i-back" viewBox="0 0 24 24"><path d="M14.5 6l-6 6 6 6" /></symbol>
      <symbol id="i-plus" viewBox="0 0 24 24"><path d="M12 5v14M5 12h14" /></symbol>
      <symbol id="i-qr" viewBox="0 0 24 24"><rect x="4" y="4" width="6" height="6" rx="1" /><rect x="14" y="4" width="6" height="6" rx="1" /><rect x="4" y="14" width="6" height="6" rx="1" /><path d="M14 14h2v2M20 14v.01M14 20h.01M17 17h3v3h-3z" /></symbol>
      <symbol id="i-copy" viewBox="0 0 24 24"><rect x="8.5" y="8.5" width="11" height="11" rx="2" /><path d="M15.5 8.5V6.5a2 2 0 0 0-2-2h-7a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h2" /></symbol>
      <symbol id="i-star" viewBox="0 0 24 24"><path d="M12 4l2.4 5 5.4.6-4 3.7 1.1 5.4L12 16l-4.9 2.7 1.1-5.4-4-3.7 5.4-.6z" /></symbol>
      <symbol id="i-check" viewBox="0 0 24 24"><path d="M5 12.5l4.5 4.5L19 7.5" /></symbol>
      <symbol id="i-shield" viewBox="0 0 24 24"><path d="M12 3l7 3v5c0 4.5-3 8-7 10-4-2-7-5.5-7-10V6zM12 8v5M12 16h.01" /></symbol>
    </svg>
  );
}

export function Icon(props: { name: IconName }) {
  return <svg class="i"><use href={`#i-${props.name}`} /></svg>;
}
