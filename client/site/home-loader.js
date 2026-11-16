// The home page as built is plain HTML: this takes the place of the app's
// script there (vite.config.ts) and loads it only when it is needed. A
// browser with machines paired needs it now, for its list; any other only
// once someone starts adding a machine.
(() => {
  const APP = '%APP%', PRELOAD = '%PRELOAD%';
  let loaded = false;
  const load = () => {
    if (loaded) return;
    loaded = true;
    for (const href of PRELOAD) {
      const link = document.createElement('link');
      link.rel = 'modulepreload';
      link.href = href;
      document.head.append(link);
    }
    import(APP);
  };
  let paired = false;
  try { paired = (localStorage.getItem('peek-machines') || '[]').length > 2; } catch {}
  if (paired) return load();
  // What is typed before the app takes the form over, for it to start from.
  const draft = window.peekDraft = {};
  const inForm = e => e.target.closest && e.target.closest('#devices.static .dadd');
  for (const type of ['pointerdown', 'focusin', 'keydown']) document.addEventListener(type, e => { if (inForm(e)) load(); });
  document.addEventListener('input', e => { if (inForm(e) && e.target.name) draft[e.target.name] = e.target.value; });
  const scan = document.querySelector('#devices.static .scan');
  if (scan && 'BarcodeDetector' in window && navigator.mediaDevices) {
    scan.hidden = false;
    scan.addEventListener('click', () => { draft.scan = true; load(); });
  }
  // The copy button, while the app is not there to do it.
  document.addEventListener('click', e => {
    const button = e.target.closest && e.target.closest('.cmd-copy');
    if (!button || loaded) return;
    navigator.clipboard.writeText(button.parentElement.querySelector('code').textContent).then(() => {
      const icon = button.querySelector('use');
      icon.setAttribute('href', '#i-check');
      setTimeout(() => icon.setAttribute('href', '#i-copy'), 1200);
    }, () => {});
  });
  if ('serviceWorker' in navigator && window.isSecureContext) navigator.serviceWorker.register('/sw.js').catch(() => {});
})();
