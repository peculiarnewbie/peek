// Peek's pipe carries a 13-byte little-endian header and a payload.
// Write logs to stderr so stdout carries only frames.
const OPEN = 1, CLOSE = 2, TEXT = 3, ABOUT = 7;
const HEADER = 13;
const { label = 'Count' } = JSON.parse(process.env.PEEK_SETTINGS || '{}');
const pages = new Set();
let count = 0;

function send(kind, channel, message) {
  const payload = Buffer.from(JSON.stringify(message));
  const header = Buffer.alloc(HEADER);
  header.writeUInt32LE(payload.length, 0);
  header.writeUInt8(kind, 4);
  header.writeBigUInt64LE(channel, 5);
  process.stdout.write(Buffer.concat([header, payload]));
}

function card(place = 'card') {
  return { t: 'view', place, view: { items: [
    { kind: 'text', text: `${label}: ${count}` },
    { kind: 'button', label: 'Increment', do: { t: 'increment' } },
  ] } };
}

function counter() {
  return { t: 'counter', label, count };
}

function receive(kind, channel, payload) {
  if (kind === CLOSE) {
    pages.delete(channel);
    return;
  }
  if (kind !== OPEN && kind !== TEXT) return;
  let message;
  try { message = JSON.parse(payload.toString()); } catch { return; }
  if (kind === OPEN) {
    if (message?.from === 'page') {
      pages.add(channel);
      send(TEXT, channel, counter());
    }
  } else if (message?.t === 'view') {
    send(TEXT, channel, card(message.place));
  } else if (message?.t === 'do' && message.do?.t === 'increment') {
    count++;
    for (const page of pages) send(TEXT, page, counter());
    if (!pages.has(channel)) send(TEXT, channel, card());
  } else if (message?.t === 'do' && pages.has(channel) && typeof message.do?.echo === 'string') {
    send(TEXT, channel, { t: 'echo', text: message.do.echo.slice(0, 500) });
  } else {
    send(TEXT, channel, { t: 'error', why: 'Unknown request.' });
  }
}

send(ABOUT, 0n, { places: ['card'], page: 'page' });
let pending = Buffer.alloc(0);
// The loop ends on stdin EOF, allowing pending stdout writes to drain.
for await (const chunk of process.stdin) {
  pending = Buffer.concat([pending, chunk]);
  while (pending.length >= HEADER) {
    const end = HEADER + pending.readUInt32LE(0);
    if (pending.length < end) break;
    receive(pending.readUInt8(4), pending.readBigUInt64LE(5), pending.subarray(HEADER, end));
    pending = pending.subarray(end);
  }
}
