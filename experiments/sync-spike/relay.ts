export type Host = { base: string; token: string };
export type RelayOptions = { delayMs?: number; dropAfterBytes?: number; onMessage?: (side: 'a' | 'b', data: string | ArrayBuffer) => void };
export async function relay(a: Host, b: Host, options: RelayOptions = {}) {
  const counts = { aToB: { messages: 0, bytes: 0, binaryBytes: 0 }, bToA: { messages: 0, bytes: 0, binaryBytes: 0 } };
  const closes: { side: string; code: number; reason: string }[] = [];
  const sockets: WebSocket[] = [];
  let binaryTotal = 0, ended = false;
  const done = Promise.withResolvers<typeof closes>();
  async function connect(host: Host) {
    const base = host.base.replace(/\/$/, '');
    const response = await fetch(`${base}/api/frame/sync-spike`, { method: 'POST', headers: { 'content-type': 'application/json', Origin: base }, body: JSON.stringify({ token: host.token }) });
    if (!response.ok) throw new Error(`ticket: ${response.status} ${await response.text()}`);
    const { ticket } = await response.json() as { ticket: string };
    return { base, ticket };
  }
  const endpoints = await Promise.all([connect(a), connect(b)]);
  for (const { base, ticket } of endpoints) {
    const ws = new WebSocket(`${base.replace(/^http/, 'ws')}/api/frame/sync-spike/ws?ticket=${encodeURIComponent(ticket)}`, { headers: { Origin: base } } as any);
    ws.binaryType = 'arraybuffer'; sockets.push(ws);
  }
  const [left, right] = sockets;
  const pending: Array<{side: 'a' | 'b'; data: string | ArrayBuffer}> = [];
  let ready = false;
  const chains = [Promise.resolve(), Promise.resolve()];
  const close = () => { for (const ws of sockets) if (ws.readyState < WebSocket.CLOSING) ws.close(); };
  function forward(side: 'a' | 'b', data: string | ArrayBuffer) {
    const i = side === 'a' ? 0 : 1, dest = i ? left : right, count = i ? counts.bToA : counts.aToB;
    options.onMessage?.(side, data);
    chains[i] = chains[i].then(async () => {
      if (options.delayMs) await Bun.sleep(options.delayMs);
      if (dest.readyState !== WebSocket.OPEN || ended) return;
      const bytes = typeof data === 'string' ? Buffer.byteLength(data) : data.byteLength;
      dest.send(data); count.messages++; count.bytes += bytes;
      if (typeof data !== 'string') { count.binaryBytes += bytes; binaryTotal += bytes; }
      if (options.dropAfterBytes && binaryTotal >= options.dropAfterBytes) { ended = true; close(); }
    });
  }
  for (const [i, ws] of sockets.entries()) {
    const side = i === 0 ? 'a' : 'b';
    ws.onmessage = e => { const m = { side, data: e.data as string | ArrayBuffer }; if (ready) forward(m.side, m.data); else pending.push(m); };
    ws.onclose = e => { closes.push({ side, code: e.code, reason: e.reason }); if (!ended) { ended = true; close(); } if (closes.length === 2) done.resolve(closes); };
  }
  try {
    await Promise.all(sockets.map(ws => new Promise<void>((resolve, reject) => { if (ws.readyState === WebSocket.OPEN) return resolve(); ws.addEventListener('open', () => resolve(), { once: true }); ws.addEventListener('error', () => reject(new Error('socket error')), { once: true }); })));
  } catch (e) { close(); throw e; }
  ready = true; for (const m of pending) forward(m.side, m.data);
  return { counts, closes, closed: done.promise, close };
}
if (import.meta.main) {
  const [baseA, tokenA, baseB, tokenB] = process.argv.slice(2);
  if (!baseA || !tokenA || !baseB || !tokenB) throw new Error('Usage: bun relay.ts URL_A TOKEN_A URL_B TOKEN_B');
  const r = await relay({ base: baseA, token: tokenA }, { base: baseB, token: tokenB });
  await r.closed; console.log(JSON.stringify({ counts: r.counts, closes: r.closes }, null, 2));
}
