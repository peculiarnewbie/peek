// Open a plugin page's socket and keep it connected while the page is open.
export function connect({ onJson, onBinary, onOpen, onClose, onFile } = {}) {
  const name = /^\/p\/([a-z0-9-]+)\//.exec(location.pathname)?.[1];
  if (!name) throw new Error('Open this page from peek at /p/<name>/.');

  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  const url = `${proto}://${location.host}/api/frame/${encodeURIComponent(name)}/ws`;
  const tellParent = t => {
    if (parent !== window) parent.postMessage({ t }, '*');
  };
  const theme = ({ source, data }) => {
    if (source !== parent || data?.t !== 'theme' || !data.vars || typeof data.vars !== 'object') return;
    for (const [key, value] of Object.entries(data.vars)) {
      if (key.startsWith('--') && typeof value === 'string') {
        document.documentElement.style.setProperty(key, value);
      }
    }
  };

  let socket;
  let stopped = false;
  let retry;
  let delay = 500;

  function open() {
    const ticket = new URLSearchParams(location.hash.slice(1)).get('ticket') ?? '';
    const ws = new WebSocket(`${url}?ticket=${encodeURIComponent(ticket)}`);
    socket = ws;
    ws.binaryType = 'arraybuffer';
    ws.onopen = event => {
      delay = 500;
      onOpen?.(event);
    };
    ws.onmessage = ({ data }) => {
      if (typeof data !== 'string') {
        onBinary?.(data);
        return;
      }
      let message;
      try { message = JSON.parse(data); } catch { return; }
      if (onFile && message?.t === 'file' && typeof message.url === 'string') onFile(message);
      else onJson?.(message);
    };
    ws.onclose = event => {
      socket = undefined;
      if (!stopped) {
        if (event.code === 4403) {
          // The parent reloads the frame with a fresh ticket.
          tellParent('renew-ticket');
        } else {
          retry = setTimeout(open, delay);
          delay = Math.min(delay * 2, 8000);
        }
      }
      onClose?.(event);
    };
  }

  window.addEventListener('message', theme);
  open();
  return {
    send(obj) {
      if (socket?.readyState !== WebSocket.OPEN) return false;
      socket.send(JSON.stringify(obj));
      return true;
    },
    sendBinary(buf) {
      if (socket?.readyState !== WebSocket.OPEN) return false;
      socket.send(buf);
      return true;
    },
    close() {
      stopped = true;
      clearTimeout(retry);
      window.removeEventListener('message', theme);
      socket?.close();
    },
    notifyChanged() { tellParent('changed'); },
  };
}
