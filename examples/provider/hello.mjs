// A provider for peek in one file: a line and a button on the machine's
// card, and a figure on its page. Run it with Node or Bun:
//
//   peek provider add hello -- node /path/to/hello.mjs
//
// then restart peek. docs/providers.md says what all of this means.

const TEXT = 3, ABOUT = 7;

/** One message to peek: u32 length, u8 kind, u64 channel, then the payload. */
function send(kind, channel, json) {
  const payload = Buffer.from(JSON.stringify(json));
  const head = Buffer.alloc(13);
  head.writeUInt32LE(payload.length, 0);
  head.writeUInt8(kind, 4);
  head.writeBigUInt64LE(channel, 5);
  process.stdout.write(Buffer.concat([head, payload]));
}

let presses = 0;

function view(place) {
  const view = place === 'card'
    ? { items: [
        { kind: 'text', tone: 'hint', text: `Hello from a provider. Pressed ${presses} times.` },
        { kind: 'button', label: 'Press', do: { t: 'press' } },
      ] }
    : { title: 'Hello', aside: 'an example', items: [
        { kind: 'tiles', tiles: [{ label: 'Presses', value: `${presses} times` }] },
      ] };
  return { t: 'view', place, view };
}

/** What a client says, and the answer on the same channel. */
function answer(ask) {
  if (ask.t === 'view' && (ask.place === 'card' || ask.place === 'page')) return view(ask.place);
  if (ask.t === 'do' && ask.do?.t === 'press') {
    presses++;
    return view('card');
  }
  return { t: 'error', why: 'hello does not know that' };
}

// Where this provider has something to show.
send(ABOUT, 0n, { places: ['card', 'page'] });

let pending = Buffer.alloc(0);
process.stdin.on('data', chunk => {
  pending = Buffer.concat([pending, chunk]);
  while (pending.length >= 13) {
    const length = pending.readUInt32LE(0);
    if (pending.length < 13 + length) break;
    const kind = pending.readUInt8(4), channel = pending.readBigUInt64LE(5);
    const payload = pending.subarray(13, 13 + length).toString();
    pending = pending.subarray(13 + length);
    if (kind !== TEXT) continue;   // a channel opening or closing: nothing to keep here
    let ask;
    try { ask = JSON.parse(payload); } catch { continue; }
    send(TEXT, channel, answer(ask));
  }
});
// peek went away.
process.stdin.on('end', () => process.exit(0));
