# Providers

A provider is a program on a machine that adds to what peek offers there,
beside the screen. peek's own are `vitals` (processor, memory, storage) and
`power` (sleep, and waking another machine). You can add your own.

A provider does two things:

- It answers requests from a paired browser, which peek passes on without
  reading them.
- It says what to show, and the client draws that from a few stock pieces.
  No code of a provider's runs in the browser.

`examples/provider/hello.mjs` is a whole provider in one file.

The parts marked **provisional** have one user so far (`power`) and may
change when a second provider uses them.

## Adding one

```
peek provider add jobs -- node C:\jobs\provider.js
peek provider list
peek provider remove jobs
```

Restart peek after a change. A name is up to 32 lowercase letters, digits
and dashes.

- peek starts the command when it starts, and again whenever it exits
  (after a second, then ever more slowly while it keeps exiting).
- **The provider runs as the same user as peek.** Under the Windows
  service that is SYSTEM, in a session with no desktop.
- The list is `providers.json` in peek's config folder: a name, and the
  command with its arguments.
- On a host started with `--view-only` the provider's environment has
  `PEEK_VIEW_ONLY=1`.

## Talking to peek

peek speaks to a provider on its stdin and hears it on its stdout. Both
carry messages of this form, numbers little-endian:

| Bytes | |
|---|---|
| 4 | length of the payload |
| 1 | kind |
| 8 | channel |
| … | the payload |

A channel is one client's conversation with the provider: a request, or a
socket for as long as it is open. Answer on the channel you were asked on.

From peek:

| Kind | | Payload |
|---|---|---|
| 1 | A channel opens | `{}`, or the state you last gave for it (see kind 5) |
| 2 | It closes | none |
| 3 | A client's message | JSON |

To peek:

| Kind | | Payload |
|---|---|---|
| 3 | A message for the client | JSON |
| 5 | How to open this channel again as it is now | JSON. If your process is replaced while the channel is open, the next one gets this as the channel's opening |
| 6 | A line for peek's log | text |
| 7 | What the provider says of itself (channel 0) | a JSON object, see below |

Send kind 7 once, as you start:

```json
{ "places": ["card", "page"], "peers": false }
```

- `places`: where you have something to show.
- `peers` (**provisional**): you want to hear of the other machines.

It appears, with your name, in the machine's `/.well-known/peek`:

```json
{ "id": "…", "name": "DESK", "protocol": 4,
  "providers": [{ "name": "screen" }, { "name": "jobs", "places": ["card", "page"] }] }
```

## How a client reaches you

Either way, peek has already checked the client: it is paired (or has the
machine's token), and it comes from a page peek trusts.

**A request**, for one answer:

```
POST /api/provider/jobs
{ "token": "…", "t": "view", "place": "card" }
```

You get the body without the token, on a channel of its own, and your first
message on that channel is the answer. You have ten seconds.

**On the client's socket**, for as long as the client wants:

```json
{ "t": "open", "p": "jobs" }
{ "t": "to", "p": "jobs", "m": { "t": "view", "place": "page" } }
{ "t": "close", "p": "jobs" }
```

You get `m`. Whatever you send on that channel reaches the client as
`{ "t": "from", "p": "jobs", "m": … }`, as often as you like. It has to be
JSON.

Your messages are your own business, apart from these, which peek and its
client understand:

| `t` | Meaning |
|---|---|
| `view` | The client asks what to show in a place |
| `do` | A button of yours was pressed |
| `peer` | The same, asked by your provider on another machine (**provisional**) |
| `error` | Your answer when you refuse: `{ "t": "error", "why": "…" }` |

A view-only host refuses `do` and `peer` itself, for every provider. It
still passes on `view`.

## Views

A client asks `{ "t": "view", "place": "card" }` and you answer:

```json
{ "t": "view", "place": "card",
  "view": { "items": [
    { "kind": "text", "tone": "hint", "text": "3 jobs waiting" },
    { "kind": "button", "label": "Run next", "do": { "t": "run" } }
  ] },
  "again": 2 }
```

- `again`: ask me again in this many seconds, for while something is under
  way. Without it the client asks every ten seconds or so.

The places:

| Place | Where | How it is asked for |
|---|---|---|
| `card` | On the machine's card in the list of machines (the hosted client) | By request, every so often |
| `page` | On the machine's own page, in the stats panel | On the socket, while the panel is open. Send a newer view on the same channel whenever it changes |

A view may have a `title` and an `aside` (shown as a heading, on the page),
and has `items`:

| `kind` | Fields | Drawn as |
|---|---|---|
| `text` | `text`; `tone`: `"hint"` or `"error"`; `busy`: true | A line. `busy` marks it as under way |
| `button` | `label`; `do`; `confirm`; `note`; `style`: `"primary"` | A button, with `note` beside it. `confirm` is asked first |
| `tiles` | `tiles`: `[{ "label", "value" }]` | Large figures. In `"12 %"`, what follows the space is set smaller |
| `meters` | `meters`: `[{ "label", "fraction", "text" }]` | Bars, `fraction` from 0 to 1 |

Anything else is left out, so an older client shows what it knows.

### Buttons

When a button is pressed you get its `do` back:

```json
{ "t": "do", "do": { "t": "run" } }
```

Answer with the view as it is now, or with an `error`, which the client
shows under the button's name.

## Other machines (provisional)

No machine knows of the others: only the browser is paired with them all.
A provider that needs the others, as `power` does to wake a sleeping one,
gets them through the browser. Say `"peers": true` of yourself, and then:

- **Records.** Put a `record` beside your card view: whatever the same
  provider on the other machines may know of this one. The browser keeps
  it, also for while this machine is away.
- **Peers.** Each `view` and `do` you are asked comes with the others:

  ```json
  { "t": "view", "place": "card", "peers": [
    { "id": "4c186f34…", "name": "LAPTOP", "up": false, "record": { … } }
  ] }
  ```

  `up` is whether that machine answers the browser now. `record` is what
  your provider there last said, or null.
- **Items about another machine.** Give an item `"about": "<that machine's
  id>"` and it is shown on that machine's card, not yours. Its button still
  comes back to you. This is how a machine that is asleep has a Wake button.
- **Asking another machine first.** A button may carry
  `"first": { "peer": "<id>", "do": { … } }`. The client sends that to your
  provider on the other machine before it sends `do` to you:

  ```json
  { "t": "peer", "from": { "id": "…", "name": "DESK" }, "do": { … } }
  ```

  If that one answers with an `error`, yours is not sent. Decide for
  yourself what another machine may ask: `power` accepts a wake and nothing
  else.

Records and `first` travel between your own paired machines, through your
browser, and nowhere else.
