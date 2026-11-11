# Extending peek beyond the screen

This is a design discussion from November 2026. Nothing in it is built. It
records what was proposed, what was rejected and why, and what is still
open, so the reasoning can be reviewed before any code is written.

## The idea

peek began as a way to see one PC's screen for a few seconds. With the
[hosted client](../README.md#one-site-for-all-your-machines) it is also a
switch between every machine on a tailnet. Once a page lists all your
machines and holds a trusted connection to each, the screen is only one of
the things worth having there:

- **Vitals.** CPU, memory and storage for every machine at a glance,
  without opening its screen.
- **Things particular to one setup.** At work, for example, what an
  automation runner on a machine is doing. That is too specific to belong
  in an open-source tool.
- **Other people.** Letting colleagues see some of it, in tiers: most may
  look at vitals, a few may take control. None of them should have to pair
  a browser with each machine.
- **Agents.** A screen and an input channel are what a computer-use agent
  needs, and the connections are already there.

The question is whether peek can host such things as separate parts,
without the person who only wants a screen paying for any of it.

## Does this exist already?

Not as one thing. As of early November 2026, from the sources at the end;
rows marked * are from memory and were not rechecked.

| Project | What it gets right | Why it is not this |
|---|---|---|
| MeshCentral | A server, an agent per machine, desktop, terminal and files in a browser, plugins | Its own accounts, device groups and relay, not a tailnet's*. Wayland capture was still in an unreleased agent branch in October. About half of 2026's commits came from one volunteer. 1.3.1 patched a server takeover that was being exploited and, per one report, broke sessions to Windows agents on servers with plugins enabled |
| NetBird browser client | SSH and RDP in a tab, from the mesh's own dashboard | Admins only, relayed only, RDP to Windows only. Not extensible |
| Cockpit* | The closest architecture: a bridge on each host, multiplexed channels, plugins that bring their own pages | Linux only, no desktop control |
| Home Assistant* | The closest extension model: devices, entities, cards, integrations | Another domain |
| tailscale-dashboard | A tailnet's device list with an SSH terminal in the browser | No screen, no plugins |
| tailscale-kvm, TailKVM | One keyboard and mouse shared across tailnet machines | Input sharing, not a screen |
| T3 Code | A machine identity that outlives its address, a list of routes to each machine, links between machines whose access can only narrow | Coding agents only |

The lightweight ones are not extensible, and the extensible one is neither
lightweight nor built around a tailnet. peek's starting point is real: it
connects instantly, the Windows binary is 3.2 MB, and it runs on Windows
and wlroots compositors.

Two extension models were looked at for how they feel to write for.
[pi](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/examples/extensions/README.md)
loads a TypeScript module that receives one API object (events, tools,
commands) and reloads it without a restart.
[opencode 2](https://dev.opencode.ai/v2/docs/build/plugins/) builds its own
features as plugins, more than eighty of them, so its plugin API is tested
by the product itself. Both run plugins inside one JavaScript process.

## The model

Six nouns, with what peek has of each today.

| Noun | Meaning | Today |
|---|---|---|
| **Node** | A machine, with an ID that outlives its address | `host-id`, made for pairing |
| **Capability** | A named thing a node offers: `screen`, `vitals`, and so on | None. `/.well-known/peek` returns the ID, name and protocol version, and would grow a list of capabilities |
| **Channel** | A typed stream belonging to one capability, many to a socket | One socket, `/ws`, with one fixed set of messages |
| **Principal and grants** | Who is connecting, and which channels they may open | See [Identity](#7-identity-has-several-sources-and-one-grant-model) |
| **Panel** | The piece of client a capability brings | One screen view, and the hosted client's devices page |
| **Agent surface** | The same capabilities as tools an agent can call | None |

Two more words are used below. The **core** is the part of peek that
accepts connections, knows who is connecting, checks grants and routes
channels. A **provider** is whatever implements a capability.

## Positions

Positions 1 and 4 are requirements set by the maintainer. The rest are
recommendations that follow from them, and are open to change in review.

### 1. peek stays simple

One small binary, one command, a screen in a browser: nothing else offers
that, and it is how peek is used every day. Extending peek must only cost
those who turn the extensions on.

- With nothing configured, peek behaves as it does now.
- The direct path stays. Opening `http://<machine>:9182/` on the tailnet
  needs no pairing, no Tailscale Serve and no hosted site.
- Grants apply once they are configured. On a personal tailnet with none,
  `--no-token` keeps meaning full control.
- Every new feature has to answer "what happens with no configuration?"
  with "peek works as before".

### 2. No new repository, and no new name, yet

A framework repository made today would be empty and would be named and
versioned before anything but peek had shaped it. Frameworks extracted
from a working product tend to hold up; ones designed first tend to be
rewritten.

- This repository becomes a workspace: the peek binary, a core crate, and
  a TypeScript package (protocol types, a provider SDK, the client's
  components).
- When the core has a user outside this repository, it moves out under a
  name of its own, and peek becomes its default distribution.
- The name peek covers looking at and touching one machine: screen, input,
  clipboard, vitals. Runner panels, jobs, an organisation's sign-in and a
  relay are not peek.
- "peek" stays out of protocol identifiers: capability names, the
  descriptor's fields, a Tailscale capability name. Those end up in other
  people's configuration and are the costly things to rename. The binary,
  the repository and the config folder can keep the name.

### 3. The core is native; providers are separate processes

| Route | Trade |
|---|---|
| **A. Native core, providers as processes speaking a local protocol, an SDK in TypeScript** (proposed) | The core stays small. A provider can crash without taking the node down, and can be written in any language. Costs a local protocol and process supervision on two operating systems |
| B. Rewrite the host on a JavaScript runtime, with plugins in its process, as pi and opencode do | The best experience for plugin authors. Every machine carries the runtime, and plugins have no isolation in a program that injects input |
| C. Embed a small JavaScript engine in the binary | Small, but npm packages and large libraries only partly work |
| D. WebAssembly components | Sandboxed. Too much for now, and no answer for panels |
| E. Adopt MeshCentral and write a plugin for it | The fastest way to features. Its identity model comes with it, and instant connect goes |

Capture, input, tile diff and JPEG encoding, about 3,200 lines, need
native code whatever is chosen. The parts that benefit from TypeScript are the SDK,
the panels and the hosted client. See [Runtime notes](#runtime-notes).

### 4. The screen is the first provider

A boundary that the main feature cannot live on is a second-class
boundary. Either it is good enough for the screen, or the design is wrong.

An earlier draft kept the screen compiled into the core, behind the same
interface but in the same process, for two reasons. Neither held up:

- **The cost of crossing.** What crosses is encoded frames, not raw
  pixels. The largest moving frame in the README's table is 250 KB, so
  about 15 MB a second at 60 frames. A local socket carries that, and the
  extra copy is small next to the JPEG encoding. The hosted path already
  puts a hop like this on every frame, through `tailscale serve`. This has
  not been measured in peek.
- **The simple install.** One binary can run both roles: the core, and a
  screen provider it starts itself. Nobody installs anything more.

There is also a reason to want the split. The screen needs a logged-in
graphical session and the node does not. With the screen as a provider
that connects to the core, the core can run as a system service: vitals
work with nobody logged in, and a failure in capture no longer takes the
node down.

What the screen requires of the boundary:

- **Binary messages from end to end.** No JSON or base64 where frames go.
- **Bounded buffering in the core.** The rule that at most two frames are
  unacknowledged has to reach the capture loop.
- **Grants as channels.** Viewing and input are separate channels, so the
  core can enforce view-only without understanding what is in a message.
- **Opening a channel is one message.** Providers stay running and idle,
  so connecting stays instant.

The check is the time to connect and the stats panel's host latency,
before and after. The host figure must run up to the core writing to the
client's socket, or it will hide what the boundary costs. If either gets
worse, the fix is a better transport for every provider (shared memory,
for instance), not an exemption for the screen.

### 5. Vitals are the second provider

The hosted client already asks each machine who it is, to show which ones
are up. That answer becomes the descriptor: host ID, capabilities, and a
snapshot of vitals. The devices page can then show vitals with no socket
open, and the first capability that is not the screen exists.

### 6. Panels are compiled into the client, never loaded at run time

The hosted client's origin holds a token for every machine a browser has
paired. Code loaded into that origin can control all of them. So
extending the client means building your own, with your panels in it, and
serving it from your own address, which `--web` and
`client/cloudflare.config.ts` already allow. The client becomes a package
others can import, and each deployment decides what goes into its build.

### 7. Identity has several sources and one grant model

Today there are three ways in. All three give full control, unless the
host runs with `--view-only`, which takes control away from everyone:

| Way in | What peek knows about who it is |
|---|---|
| The access token | Nothing |
| A tailnet address, with `--no-token` | Nothing: `tailnet()` checks the address range only |
| A paired browser | An ID and a label, and it can be unpaired on its own |

Each becomes a source of principals, and grants say which channels a
principal may open.

- **Paired browsers.** The record gains grants: `peek pair --view-only`,
  say. This is the cheapest tier to build.
- **Tailscale.** Asking the local Tailscale client who is behind an
  address gives a user and a device. The tailnet's policy can then carry
  the grants as an
  [application capability](https://tailscale.com/docs/features/access-control/grants/grants-app-capabilities),
  with no user management in peek. The shape below is only an
  illustration:

  ```json
  {
    "src": ["group:eng"],
    "dst": ["tag:peek"],
    "app": { "example.com/cap/node": [{ "channels": ["vitals", "screen.view"] }] }
  }
  ```

- **A sign-in from elsewhere.** For people who are not on the tailnet, the
  machine dials out to a relay, they sign in to an existing web
  application, and the relay passes on only what their role allows. A
  relay was considered and turned down for the hosted client, because
  every frame would pass through it. It is worth that cost only where the
  viewers cannot be on the tailnet.

Tiers mean nothing while the `--no-token` direct path gives full control
to anyone on the tailnet. Once grants are configured on a machine, that
path has to identify the caller through Tailscale or be closed.

### 8. Private capabilities live outside this repository

A capability that only makes sense in one organisation is a provider and
a panel in that organisation's own repository, built into its own client.
peek is not forked, and never learns what the capability means.

The case that started this: an automation system whose runners already
report their status to an orchestrator on a heartbeat. Showing that
status, and the machine's vitals, belongs in that system's own dashboard
and needs nothing from peek. peek is useful there for seeing and
controlling the runner machine's screen, and as the place where the
runner can offer a capability on its node, so that its panel sits beside
the screen.

## Order of work

1. Split peek into a core and a screen provider over a local protocol,
   within one binary. Measure before and after.
2. The descriptor, with vitals as the second provider and the devices
   page showing them.
3. Grants on pairing records, and Tailscale identity for the direct path.
4. The provider SDK, and the first provider outside this repository.
5. A relay route and an adapter for an outside sign-in.
6. The agent surface.

Whether to call any of this a framework is a question for after step 4.

## Risks

- **Building the framework first.** No plugin written for opencode 1
  carried over to version 2 unchanged. The split in step 1 is taken from
  the screen as it exists; everything after it should start as a concrete
  feature too.
- **A change of category.** A program that runs other people's code and
  injects input across a set of machines is what attackers look for.
  Before this is offered to anyone running machines for others, it needs
  an audit log, grants enforced on the node and not only in a relay, and
  signed providers.
- **Scope.** Five times the work is the floor for one maintainer.
- **Step 1 ships nothing new.** A local protocol and process supervision
  on two operating systems come before any feature.

## Open questions

- The local transport on each system (a Unix socket, a named pipe), its
  framing, and whether shared memory is ever needed.
- How a core running as a system service and a screen provider running in
  a user's session find and trust each other.
- Whether the core starts the providers bundled with it, or every
  provider connects on its own.
- What happens to the `--no-token` direct path once a machine has grants.
- Where the list of machines comes from: the browser's storage, as now,
  or a registry an organisation already keeps.
- Whether vitals are a snapshot in the descriptor, a live channel, or
  both.
- `/.well-known/peek` already carries the name. Whether the descriptor
  moves before anything else depends on it.
- The shape of the agent surface, and what an agent's grants look like.
- The name of the core, when it leaves this repository.

## Runtime notes

- `peek.exe`, release build: 3.2 MB. One reading of a running instance on
  Windows: a working set of about 26 MB.
- A command-line tool from another project, compiled with Bun on the same
  machine: about 100 MB.
- Bun's ahead-of-time compiler (`bun compile`) was experimental and in
  canary builds only, and could not build for another platform. Two other
  projects reported 52 and 64 MB on disk for small tools with `--trim`,
  and the first about 20 MB in memory.
- Porffor still called itself alpha. By its own figures about 80% of the
  Effect test suite passed, so a provider built on a large library would
  not run on it reliably.

A JavaScript core is plausible within a year and is about twenty times
peek's size today. The protocol is what should last; the language either
side of it can change.

## Sources

- MeshCentral:
  [issue 2719](https://github.com/Ylianst/MeshCentral/issues/2719),
  [issue 7875](https://github.com/Ylianst/MeshCentral/issues/7875),
  [a 2026 guide](https://www.openmsp.ai/blog/meshcentral-guide)
- NetBird:
  [browser client](https://docs.netbird.io/manage/peers/browser-client),
  [its architecture](https://docs.netbird.io/manage/peers/browser-client/architecture)
- Tailscale:
  [application capabilities](https://tailscale.com/docs/features/access-control/grants/grants-app-capabilities),
  [grants](https://tailscale.com/blog/grants-ga)
- Tailnet tools:
  [tailscale-dashboard](https://github.com/all3n1k/tailscale-dashboard),
  [tailscale-kvm](https://github.com/ryguy0601/tailscale-kvm),
  [TailKVM](https://github.com/sasanoha-tk/TailKVM)
- T3 Code:
  [remote internals](https://github.com/pingdotgg/t3code/blob/main/docs/internals/remote.md)
- Extension models:
  [opencode plugins](https://dev.opencode.ai/v2/docs/build/plugins/),
  [moving from opencode 1 to 2](https://kimaki.dev/blog/opencode-v2-migration),
  [pi extensions](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/examples/extensions/README.md)
- Runtimes:
  [Bun ahead-of-time compilation](https://bun.com/docs/compiler/aot),
  [smaller Bun executables](https://bun.com/docs/compiler/size),
  [the Bun pull request](https://github.com/oven-sh/bun/pull/45631),
  [Porffor](https://porffor.dev/)
