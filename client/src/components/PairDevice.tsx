// "Pair a device": the host makes a one-time code and a link to the hosted
// client that uses it, shown here as a QR code for a phone to scan.
import { createSignal, onCleanup, Show } from 'solid-js';
import { renderSVG } from 'uqr';
import { hooks, send } from '../connection';

export interface PairOffer {
  code: string;
  expires: number;
  host: string | null;
  name: string;
  web: string | null;
  link: string | null;
  /** The same code on the client this machine serves itself. */
  own?: string | null;
  error?: string;
}

const [offer, setOffer] = createSignal<(PairOffer & { until: number }) | null>(null);
const [now, setNow] = createSignal(Date.now());
hooks.paircode = o => setOffer({ ...o, until: Date.now() + o.expires * 1000 });

export function PairDevice() {
  const timer = setInterval(() => setNow(Date.now()), 1000);
  onCleanup(() => clearInterval(timer));
  const left = () => Math.max(0, Math.round(((offer()?.until ?? 0) - now()) / 1000));
  const expired = () => !!offer() && left() === 0;
  return (
    <div class="pair">
      <Show when={offer() && !expired()} fallback={
        <button class="btn wide" tabIndex={-1} onClick={() => send({ t: 'paircode' })}>
          {expired() ? 'Code expired: show a new one' : 'Show a pairing code'}
        </button>
      }>
        {(() => {
          const o = offer()!;
          return (
            <Show when={!o.error} fallback={<p class="err">{o.error}</p>}>
              <Show when={o.link ?? o.own} fallback={
                <p class="hint">Tailscale is not running on {o.name}, so other devices cannot reach it through the
                  peek site. Code: <code>{o.code}</code></p>
              }>
                <div class="qr" innerHTML={renderSVG((o.link ?? o.own)!, { border: 2 })} />
                <Show when={o.link} fallback={<p class="hint">Scan it on the other device, or open <code>{o.own}</code> there.</p>}>
                  <p class="hint">Scan it on the other device, or open <code>{o.web}</code> there and add{' '}
                    <code>{o.host}</code> with code <code>{o.code}</code>.</p>
                  <Show when={o.own}><p class="hint small">Or open {o.name}’s own page: <code>{o.own}</code></p></Show>
                </Show>
              </Show>
              <p class="hint small">Works once, for {Math.floor(left() / 60)}:{String(left() % 60).padStart(2, '0')}.</p>
            </Show>
          );
        })()}
      </Show>
    </div>
  );
}
