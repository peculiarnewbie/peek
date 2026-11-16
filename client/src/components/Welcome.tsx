// The hosted site's home for a browser with no machines paired: what peek is,
// and the form to add one. Rendered when the site is built, so it must not
// touch the browser as it loads; the page takes it over in place once someone
// starts using the form (site/home-loader.js), keeping what was typed and
// where. Pairing itself, and the QR scanner, load only when used.
import { createSignal, lazy, onSettled, Show } from 'solid-js';
import { cleanHost, parseLink } from '../links';
import { AddForm, Foot, Header, Intro } from './Intro';

const QrScan = lazy(() => import('./QrScan').then(m => ({ default: m.QrScan })));

/** What the page's loader kept of the form before this took it over. */
interface Draft { machine?: string; code?: string; scan?: boolean }
const draft = (): Draft => (typeof window === 'undefined' ? null : (window as { peekDraft?: Draft }).peekDraft) ?? {};

export function Welcome() {
  const d = draft();
  const [host, setHost] = createSignal(d.machine ?? '');
  const [code, setCode] = createSignal(d.code ?? '');
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal('');
  const [scanning, setScanning] = createSignal(false);
  // Whether to offer the camera: 'maybe' until the browser says.
  const [scan, setScan] = createSignal<boolean | 'maybe'>('maybe');
  onSettled(() => {
    if (draft().scan) setScanning(true);
    if (typeof BarcodeDetector === 'undefined' || !('mediaDevices' in navigator)) setScan(false);
    else BarcodeDetector.getSupportedFormats().then(f => setScan(f.includes('qr_code')), () => setScan(false));
    delete (window as { peekDraft?: Draft }).peekDraft;
  });
  const onHost = (value: string) => {
    const link = parseLink(value);
    if (link) { setHost(link.host); setCode(link.code); } else setHost(value);
  };
  async function submit(h: string, c: string) {
    setBusy(true);
    setError('');
    try {
      const hosted = await import('../hosted');
      hosted.openMachine(await hosted.pair(h, c));
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  return (
    <main id="devices" class="static">
      <Header />
      <Intro />
      <AddForm host={host()} code={code()} busy={busy()} error={error()}
        ready={!busy() && !!cleanHost(host()) && !!code().trim()} scan={scan()}
        onHost={onHost} onCode={setCode}
        onSubmit={() => { if (!busy()) void submit(host(), code()); }}
        onScan={() => { setError(''); setScanning(true); }} />
      <Show when={scanning()}>
        <QrScan onClose={() => setScanning(false)} onLink={(h, c) => {
          setScanning(false);
          setHost(h);
          setCode(c);
          void submit(h, c);
        }} />
      </Show>
      <Foot />
    </main>
  );
}

interface Detector { detect(source: HTMLVideoElement): Promise<{ rawValue: string }[]> }
declare const BarcodeDetector: { new (options: { formats: string[] }): Detector; getSupportedFormats(): Promise<string[]> };
