// peek's service worker, there so browsers offer to install peek as an app.
// It caches nothing: the client is only useful connected to its host, and
// the host always serves the client that matches it. When the host cannot
// be reached, opening the app says so instead of showing a browser error.
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', e => e.waitUntil(self.clients.claim()));
self.addEventListener('fetch', e => {
  if (e.request.mode !== 'navigate') return;
  e.respondWith(fetch(e.request).catch(() => new Response(
    '<!doctype html><meta name="viewport" content="width=device-width"><title>peek</title>' +
    '<body style="margin:0;height:100vh;display:grid;place-items:center;background:#0d0d0d;color:#c3c2b7;' +
    'font:15px system-ui,sans-serif;text-align:center;padding:0 24px">' +
    '<div>The host is not reachable.<br>Check that it is on and that Tailscale is connected, then reopen peek.</div>',
    { headers: { 'content-type': 'text/html; charset=utf-8' } })));
});
