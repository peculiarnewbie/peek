// Hosted client: pairing by pointing the camera at the QR code `peek pair`
// shows. Only where the browser reads QR codes itself (Chrome on Android,
// among others); elsewhere the camera app opens the link just as well.
import { createSignal, onCleanup, onMount } from 'solid-js';
import { parseLink } from '../hosted';

interface Detector { detect(source: HTMLVideoElement): Promise<{ rawValue: string }[]> }
declare const BarcodeDetector: {
  new (options: { formats: string[] }): Detector;
  getSupportedFormats(): Promise<string[]>;
};

export const [canScan, setCanScan] = createSignal(false);
if (typeof BarcodeDetector !== 'undefined' && 'mediaDevices' in navigator) {
  BarcodeDetector.getSupportedFormats().then(f => setCanScan(f.includes('qr_code')), () => {});
}

const AIM = 'Point the camera at the QR code that peek pair shows.';

export function QrScan(props: { onLink: (host: string, code: string) => void; onClose: () => void }) {
  let video!: HTMLVideoElement;
  const [note, setNote] = createSignal(AIM);
  let stream: MediaStream | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let stopped = false;
  const stop = () => {
    stopped = true;
    clearTimeout(timer);
    stream?.getTracks().forEach(t => t.stop());
  };
  onCleanup(stop);

  onMount(async () => {
    try {
      stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: { ideal: 'environment' } }, audio: false });
    } catch (e) {
      setNote((e as Error).name === 'NotAllowedError'
        ? 'Camera access is blocked for this site. Allow it, or scan the code with the camera app instead.'
        : 'No camera is available.');
      return;
    }
    if (stopped) { stop(); return; }
    video.srcObject = stream;
    video.play().catch(() => {});
    const detector = new BarcodeDetector({ formats: ['qr_code'] });
    const look = async () => {
      if (stopped) return;
      if (video.readyState >= 2) {
        try {
          for (const { rawValue } of await detector.detect(video)) {
            // Only this site's own pairing links: a code for another site would not work here anyway.
            const link = parseLink(rawValue);
            if (link && new URL(rawValue).origin === location.origin) {
              stop();
              navigator.vibrate?.(15);
              props.onLink(link.host, link.code);
              return;
            }
            setNote('That QR code is not a peek pairing link. ' + AIM);
          }
        } catch {}
      }
      timer = setTimeout(look, 150);
    };
    look();
  });

  return (
    <div id="qrscan" role="dialog" aria-label="Scan a pairing QR code">
      <video ref={video} muted playsinline />
      <div class="qrframe" />
      <p class="qrnote">{note()}</p>
      <button class="btn alt" tabIndex={-1} onClick={() => { stop(); props.onClose(); }}>Cancel</button>
    </div>
  );
}
