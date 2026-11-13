# Plugin foundation: implementation and release gates

13 November 2026. Work branch: `feat/plugin-foundation-screen`.

This branch includes a copy of the uncommitted `spike-frames` implementation
and hardening on top. The original `D:/git/peek-frames` checkout is untouched.
The package is still version 0.2.2; this work does not publish a 0.3.0 release.

## What is implemented

- Plugin manifests, settings, dependency checks, process supervision, recent
  logs, and live install/enable/disable/restart/remove management from the spike.
- Custom pages, provider-scoped tickets, binary page sockets, and streamed
  single-use downloads from the spike.
- Explicit manifest/pipe contract version 1. Omitted version fields mean 1
  for existing spike packages. Unsupported versions, invalid names/commands,
  invalid setting defaults, absent required programs, and missing executables
  are refused before a plugin is installed.
- Local `.tgz` package installation, as well as folders, HTTPS git sources,
  and npm names. Package fetch and dependency installation disable npm scripts.
  Installing a plugin still explicitly authorizes its program to run.
- Plugin stop/restart closes old page channels, allowing clients to reconnect
  to the replacement link. A cancelled one-shot request closes its channel.
- A page socket has an eight-message output queue. Overflow closes it instead
  of accumulating an unlimited backlog or silently dropping application data.
  Socket writes and idle reads have deadlines. The built-in screen path is
  unchanged by this queue policy.
- Frame tickets are renewed after the main connection recovers. The frame is
  recreated for a new ticket: changing only its fragment would leave the page
  running with the old ticket. Brief provider startup races get bounded retries.
- A page can request renewal with `parent.postMessage({t:'renew-ticket'}, '*')`.
  The parent validates the sending frame and origin and never sends its machine
  token into the page. Renewal reloads the page; apps own durable UI/app state.
- Expired tickets are pruned when new tickets are issued. Management actions
  lose their pending answer on disconnect and are not automatically replayed.
- Page assets are checked after resolving directory links and junctions.
  Provider changes refresh the machine-served client's descriptor too.
- Opening a provider page pauses the underlying built-in screen, including
  when the machine-served client shows a page in a panel.
- `PEEK_CONFIG_DIR` isolates development/test hosts and their management CLI
  from the installed host's configuration and secrets.

## Authoring contract

A minimal manifest:

```json
{
  "manifest_version": 1,
  "provider_protocol": 1,
  "name": "my-app",
  "version": "1.0.0",
  "description": "What the app does.",
  "run": ["bin/my-app"],
  "needs": []
}
```

`run` is an argument array, not a shell command. Its working directory is the
installed plugin directory. Bundle a binary for the installation's OS/CPU,
or declare the runtime in `needs`. Settings arrive in `PEEK_SETTINGS`; a
view-only host also sets `PEEK_VIEW_ONLY=1`. The provider must honor that mode
for its own protocols: the host recognizes conventional `do`/`peer` actions
and rejects inbound binary, but cannot infer effects of arbitrary messages.

`version` is an optional string, shown as given. `PEEK_DATA_DIR` names a
folder for the plugin's persistent data, kept apart from its installed files.
Updating keeps valid settings, the on/off state and that data. Removing keeps
the data unless `--purge` is given. Peek does not check for newer versions or
update automatically.

The pipe format and view/page conventions are in [providers.md](providers.md)
and [frames.md](frames.md). Stdout is only for protocol frames; use the LOG
message for logs. Exit when stdin ends. The host can reopen a live channel
with its last STATE after a process replacement. This is not durable job
storage, guaranteed delivery, or automatic retry of mutations.

Pages and trusted native plugins are separate boundaries. A frame ticket
limits access to a provider socket. It does not sandbox the native process,
which runs with Peek's OS account rights (SYSTEM under the Windows service).
The machine-served client and plugin pages share an origin; the hosted client
has a separate origin. Static page assets remain publicly fetchable to callers
that can reach the node; application data belongs behind authenticated channels.

## Reproducible checks

From the repository root, on Windows:

```powershell
cargo test
cargo build --release
cargo build --release --manifest-path examples/provider/files/Cargo.toml
node experiments/screen-plugin/build.mjs
bun bench/plugins.ts --browser --screen
bun bench/screen-plugin.ts 30
```

The end-to-end checks create two independent hosts, configurations and tokens
in unique temporary directories. They use loopback and never modify the
installed service. The optional browser checks launch headless Chrome with a
temporary profile, not the user's browser. Set `PEEK_TEST_CHROME` to override
the executable. No test injects desktop input or signs the user out.

Verified here:

- 64 Windows host Rust tests and two native file-plugin tests pass; two
  existing host capture/benchmark tests remain ignored. Normal, hosted, and
  experimental client builds and TypeScript checks pass.
- A local npm tarball installs through the real CLI without lifecycle scripts.
- A native file plugin installs through the management socket, saves shared
  folder settings, lists files, refuses traversal, and returns a verified
  Unicode-named download whose link works only once.
- Independent node identity/authentication, wrong-provider ticket refusal,
  page text/binary, restart/reconnect, stop/start, view-only and remote-install
  policy, removal, and renewed authorization after host restart.
- A directory junction cannot serve a page asset outside the page root.
- Headless Chrome renders the built-in desktop, opens a notes page, recovers
  its live socket after a full host restart, and closes the frame.
- The experimental screen renders in a real iframe over its generic socket,
  and pause/resume controls work. See the experiment's report for scope/costs.

## Remaining release gates

These are not silently covered by the local tests:

- Two physical machines over Tailscale, both the hosted and direct clients,
  including non-default Serve ports and local-network permission prompts.
- Firefox and mobile Safari, touch/keyboard/focus/fullscreen/clipboard behavior.
- Windows installed-service behavior with this branch, including recovery at
  login/lock/UAC/session change. Testing does not replace or restart the user's
  current service, lock their desktop, or sign them out.
- Linux/Wayland verification. The local Ubuntu WSL installation could not start
  because its configured virtual disk was missing; no repair was attempted.
- Publish a supported OS/CPU package and test installation by its registry/git
  identity. The local package distribution path is tested; no public package
  or host release was published.

The repository's default `cargo fmt --check` currently reports formatting
changes throughout the existing codebase. This branch does not reformat the
whole repository as part of plugin work.

Keep data migration, fleet enrollment, agent tool discovery, and sync
out of this release. They are later slices in [the roadmap](vision-and-roadmap.md).
