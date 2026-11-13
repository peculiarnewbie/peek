# Peek: vision and game plan

Discussion summary and proposed implementation sequence, 13 November 2026.
This records the direction we explored; it is not a promise of features or release dates.

## The product

**All your computers, one place.** Access desktops, check on your machines,
and add tools that make them yours.

Peek starts as a useful fleet management tool suite: desktop access, machine
health, power controls, and files. People can use the suite without knowing
that its features are plugins. Developers can build independent apps on the
same foundation, with their own interfaces and protocols.

First-party apps ship inside Peek. Someone with several machines and no
interest in development should get desktop access, health, power, files, and
sync without installing Node, git, or npm. Those apps use the same manifest
and provider protocol as an outside plugin, and are bundled in the executable
the way vitals and power are today. An app that is turned off starts no
process.

The larger opportunity: if personal apps become cheap to create, giving them
a consistent home becomes valuable. Each app should not have to reinvent
installation, process supervision, remote access, settings, or presentation.
An app can run on one machine or coordinate several, and can offer a human
interface, programmable operations, or both.

Keep the name Peek for now. Use **apps** when describing what people install;
use **plugins** and **providers** for the implementation contract.

## The small kernel

Peek accepts authenticated connections, supervises providers, advertises
capabilities, routes messages and binary streams, and serves interfaces.
Extend these responsibilities only when a real app demonstrates a missing
shared primitive. Preserve the simple desktop-only installation and its
connection speed and idle resource use.

The kernel does not need a universal model of bots, jobs, work sessions,
render queues, media libraries, or synchronized folders. Plugins own those
concepts and their durable state. External programs and headless services
should be able to use the same public interfaces as the browser.

Use the existing network underneath Peek. Tailscale integration should avoid
duplicating connectivity and identity machinery where it can supply them.
Keep direct access and existing pairing workflows working. Peer access starts
from Tailscale identity (slice 4); what that cannot cover still needs design.

Screen should become a bundled, default-on plugin using the same public
contracts as other plugins. A single executable can still ship both host
and bundled provider roles. Default installation policy and privileged
launch authorization are host concerns, not reasons for screen-specific
message routing.

## What we learned from related tools

SSH can supply the ingredients. Peek makes their packaging and use consistent
across many small apps. It need not replace SSH to be useful.

Executor is a useful analogy for bringing integrations behind a shared
interface; Peek's focus is apps and capabilities running on your computers.
Rex is a close architectural relative, organized around durable sessions
and blocks. Peek's proposed contract is independently running providers.
Cockpit and MeshCentral are precedents, so the underlying architecture is
not unprecedented. The opportunity is the combination of a small host,
useful default suite, browser access, and approachable app packaging.

These comparisons are positioning context, not requirements to copy their
feature sets or claims that their future scopes cannot overlap Peek.

## Current baseline

The plugin work is on the branch `feat/plugin-foundation-screen`: the spike,
and the hardening recorded in [plugin-release.md](plugin-release.md). Its
package version still reads 0.2.2, and nothing is released. Do not confuse
its behavior with released guarantees.

Files is built in, uses the same manifest, and is off by default.

It already has:

- Process providers with a framed stdin/stdout protocol, restart behavior,
  channel state restoration, descriptors, logs, and management controls.
- Plugin folders and manifests; settings; dependency checks; installation
  paths for local folders, git, and npm; live enable/disable and restart.
- Provider-owned HTML pages with text/binary sockets, provider-scoped frame
  tickets, and downloadable file offers.
- Vitals and power providers, a read-only file app, and a sample notes page.
- A Windows service/core split that keeps connections across screen-provider
  replacement and desktop session changes.

Important limits:

- Fleet records and provisional cross-machine actions are carried by the
  browser. Providers have no shared, unattended peer-access facility.
- Screen has separate registration, launch, transport, and client paths.
- Plugins are trusted native programs, running with Peek's account rights;
  under the Windows service this is SYSTEM. Process separation is not a
  security sandbox, and frame tickets do not restrict native process rights.
- View-only relies partly on recognized message names, not a complete model
  of arbitrary plugin effects. Caller-specific provider grants are missing.
- Updates, data preservation conventions, and platform package selection are
  unfinished. Tickets are lost on host restart. Cross-machine and browser
  coverage remains incomplete.
- Channel restoration is not durable app state or guaranteed task delivery.

See [providers.md](providers.md) and [frames.md](frames.md) for the exact
contract and recorded checks.

## Delivery sequence

Slice boundaries are proposed. Only the next plugin release has a tentative
version association; later slices should be sized after the earlier ones work.

### 1. Turn the plugin spike into a dependable first release

Review the existing stack and integrate its work in dependency order. Keep
scope centered on the default suite and one independently packaged file app.

- Document manifest/protocol compatibility and unsupported-version behavior.
- Validate installation, settings, logs, restart, removal, and dependency
  errors in the supported host configurations.
- Exercise two actual machines, the direct and hosted clients, Firefox,
  mobile Safari, and host/service restarts. Verify ticket renewal and screen
  visibility behavior rather than relying on incidental background-tab state.
- Publish and install one real package through an advertised distribution
  path. State which OS/architecture combinations it supports.
- Make execution identity and trusted-plugin privileges visible at install.

**Done when:** a fresh supported machine can install Peek, use the default
suite, install the file app, and manage it without rebuilding the client or
manually repairing state. No regression in desktop connection behavior.

### 2. Make screen a bundled plugin

- Use the provider registry and lifecycle contract for screen registration.
- Generalize session-aware launch as an explicitly authorized host policy.
  Preserve Windows login, lock-screen, UAC, and session-switch behavior.
- Provide generic binary transport without routing all unknown messages to
  screen. Measure latency, slow-client buffering, and idle resource use.
- Move screen UI through the app boundary, introducing only the narrow host
  hooks actually needed for input, focus, visibility, clipboard, and navigation.
- Keep recovery possible when a screen plugin fails or is misconfigured.
  Decide disable/remove policy separately from its default-on installation.

**Done when:** the default screen implementation uses documented extension
interfaces and a second screen provider could be implemented outside the repo.
Performance and desktop-session behavior remain acceptable against baseline.

### 3. Finish the app lifecycle and authoring contract

- Separate installed code from persistent app data and configuration.
- Add versioned updates with data preservation, failed-update recovery, and
  explicit uninstall/data-retention behavior.
- Define OS/architecture package selection and runtime requirements.
- Support service and appropriate user-session execution contexts without
  silently assuming a service shares a desktop user's credentials.
- Supply a small SDK/example and document connection errors, shutdown,
  readiness, and bounded buffering. Apps own database migrations and jobs.

**Done when:** an independently authored app can be installed, updated,
restarted, and removed predictably, with data kept according to its policy.

### 4. Enable authorized peer access without an open browser

Use the sync app's needs to shape this interface; do not design a universal
fleet controller first.

Start from Tailscale. Both machines are already on the tailnet, so a person
should not have to pair them a second time. Ask the local Tailscale client
who is calling, and take grants from tailnet policy (application
capabilities), as position 7 of the extensibility discussion (PR 2) proposes.
Build Peek's own enrollment only for what that cannot cover, and only when a
real case needs it.

- Verify first what identity reaches Peek on each path. Behind Tailscale
  Serve the connection arrives from the proxy, so the caller has to come from
  what Serve passes on; on the direct path, from asking the local client
  about the caller's address. Neither has been tried.
- Identify a headless caller by its tailnet node and user, and discover its
  capabilities from the descriptor. A machine list does not itself authorize
  access.
- Scope access to the destination provider/capability; transmit authenticated
  caller context and enforce grants on the receiver.
- Define how direct access behaves once narrower grants are configured.
- Let providers request an authorized connection without receiving every
  machine's unrestricted token. Keep UI tickets separate from service access.
- Define offline status, reconnect behavior, and access revocation. Record
  relevant access and management events with caller identity.
- Establish a minimal policy for authenticated bulk streams. First reuse
  existing channels; add HTTP endpoints only if an actual workload needs them.
- Add the narrow page-to-client bridge here, not in slice 6. Sync's page has
  to offer "pick the other machine", and only the browser knows the person's
  machines. A page can ask the client which machines there are and ask it to
  connect two of them. Machine tokens stay outside plugin frames.

The first permission boundary can be whole-provider access. Finer operation
permissions require explicit operations, not guessing effects from payloads.
Trusted native plugins can still exercise their OS account rights: restricting
local plugin code requires separate OS isolation, not merely protocol grants.

**Done when:** a provider on A reaches an explicitly authorized provider on B
with every browser closed; an unauthorized provider is refused, revocation
works, and a network interruption can be recovered without reenrollment. A
page can offer the person's other machines without holding their tokens.

### 5. Build the self-contained folder-sync app

This is an original sync implementation packaged as a Peek plugin, not a
wrapper around an installed Syncthing service. It owns its engine, state,
protocol, settings, and UI. Platform binaries may differ; the app's normal
installation should bring what it needs.

Suggested progression, subject to a focused sync design:

1. One-way replication between two explicitly selected folders. Index files,
   transfer in bounded chunks, resume interruptions, verify content, and use
   temporary files before replacing destinations. Preserve destination-only
   files initially; make deletion propagation a separate explicit behavior.
2. Make recovery trustworthy: durable progress, offline catch-up, rescans,
   changed-during-read handling, and defined path/link, permissions, and
   case-sensitivity behavior across supported filesystems.
3. Add two-way reconciliation with change history, explicit deletion handling,
   and preserved conflict copies. Do not silently resolve concurrent edits.

Use the app to discover missing host primitives, but keep indexes, conflict
policy, reconciliation, retries, and transfer scheduling inside the app.

**Done when:** one-way replication is trustworthy (steps 1 and 2): an initial
copy completes with the browser closed, and interrupted copies resume to
verified contents. This is the gate for the slices after it.

### 5b. Two-way sync, on its own clock

Step 3 is a slice of its own and gates nothing after it. For people who only
want their machines to work, one lost file costs trust in the whole suite. So
two-way ships when it survives simultaneous edits and offline deletions
without silent data loss, however long that takes. Until then, one-way
already covers backing up or mirroring a folder to another machine.

### 6. Make the suite feel like one product

Use desktop, health/power, files, and sync as the initial suite. Provide clear
machine/app navigation, useful availability/error states, deep links, and an
install/manage experience that does not expose internal provider terminology.

Add a fleet-level app entry point, building on the page-to-client bridge from
slice 4. Test access from a second device, including a phone. Fleet install/update
operations can follow real demand and should report partial success honestly.

**Done when:** a person can find a machine, open its desktop, browse a share,
configure sync, and understand a disconnected machine without knowing the
implementation architecture.

### 7. Validate reuse with another workload and a tool surface

This slice does not wait for two-way sync (5b).

A Blender render app is a useful candidate, not a prerequisite for every
release. Start with submitting a background render and retrieving verified
results. Let its coordinator own queueing, scheduling, task identity, retries,
and cancellation. Live interactive Blender control can come later.

Define optional discoverable operations with schemas and authenticated calls.
Allow headless clients and, through an adapter, agent access. Keep agents
subject to the same grants; do not require every app to implement AI features.

**Done when:** another developer can build the app outside Peek's repository,
coordinate two workers, and expose useful operations without core changes
specific to Blender. Media serving is a further architecture check, not a
commitment to build a Plex replacement.

### 8. Peekies, last

Build bot orchestration as an app on the proven foundation. Connect to T3
through a separate adapter, after verifying its supported external interface.
Peekies owns bot definitions, memory, workflow state, routines, and delegation.
Keep its design deferred until earlier apps establish the actual interfaces.

**Done when:** a bot can dispatch work, outlive the browser, recover results,
and open the relevant machine for human intervention without Peekies-specific
logic in the kernel.

## What to do next

Finish slice 1: its remaining release gates are in
[plugin-release.md](plugin-release.md), starting with two physical machines
and the installed service. In parallel with design, specify the smallest
two-node sync interaction needed for slice 4; do not implement the sync engine
or Peekies as part of the first plugin release.

At each slice, ask whether a new requirement belongs to the host or the app.
Keep host additions tied to a concrete consumer and a verification scenario.
Avoid extracting a separate framework repository or freezing a broad SDK
before independently authored apps have exercised the boundary.
