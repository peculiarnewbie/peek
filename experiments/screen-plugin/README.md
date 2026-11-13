# Screen as a normal plugin: viability spike

Opt-in only. The default screen implementation and Windows service launcher
remain built in. This experiment is evidence for deciding whether extraction
is worth doing, not a commitment to ship it.

## What it proves

The existing native capture/input engine can run as an ordinary plugin process,
advertise a page, and use the generic provider-page socket for its text and
binary traffic. The screen page is a separate build of the existing UI, using
a provider ticket rather than the main screen socket or its machine token.

The generated folder has a manifest, a bundled executable, and one HTML page.
It installs through the normal plugin manager as `screen-experiment` and can
be stopped, restarted, and removed without removing the built-in screen.

Production changes needed for this prototype are small: an internal
`--provider-page` option, honoring `PEEK_VIEW_ONLY` in the bundled provider,
and app-level timing probes. The separate UI build has a transport switch and
omits pairing, nested plugin pages, and service-worker registration. Existing
screen controls, renderer, touch/input code, and stats are reused, not rewritten.

## Build and try

```powershell
cargo build --release
node experiments/screen-plugin/build.mjs
```

Start an isolated interactive host in a terminal:

```powershell
$env:PEEK_CONFIG_DIR = Join-Path $env:TEMP ('peek-screen-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $env:PEEK_CONFIG_DIR
target/release/peek.exe --bind 127.0.0.1 --port 19182 --web none
```

Open the token-bearing URL it prints. In its Plugins page, install the
absolute path of `experiments/screen-plugin/package`. Open `screen-experiment`
from the Pages section of Stats. Keep the isolated configuration path if you
want to manage the same test host through the CLI in another terminal.

The host must run in the interactive desktop session. Do not install this
prototype into the normal SYSTEM service and expect it to reach the desktop.
The generated package contains the binary for the machine it was built on.

## Checks and measured cost

```powershell
cargo build --release --manifest-path apps/files/Cargo.toml
bun bench/plugins.ts --browser --screen
bun bench/screen-plugin.ts 30
bun bench/screen-plugin.ts 3 --cold
```

The warm benchmark alternates built-in and plugin connections, warming both
capture engines first. It excludes the ticket HTTP request and browser decode,
but includes pipe/socket delivery in first-frame timing. Both paths have
clipboard disabled for that benchmark. No keyboard/mouse input is injected.

One unloaded Windows loopback run, 30 samples per path, 1920 x 1080:

| Measurement | Built-in | Plugin |
| --- | ---: | ---: |
| Median first complete frame | 5.81 ms | 5.82 ms |
| p90 first complete frame | 6.96 ms | 8.77 ms |
| Median frame size | 307,358 bytes | 307,358 bytes |

Earlier runs showed median differences around 0.25–0.37 ms and variable tail
latency. This is evidence that the warm transport is plausible, not a guarantee
of zero overhead. A run concurrent with compilation was discarded for latency
comparison. The frame's own host timing does not include every IPC/queue cost;
do not use that field alone to judge the boundary.

The stream check forces 60 image updates by changing quality, acknowledges
each, then verifies visibility pause and a timing probe. This is not a video
or sustained desktop-motion benchmark. Headless Chrome additionally verifies
actual iframe rendering and pause/resume on the ordinary ticketed socket.

A separate cold check, with no concurrent compilation and 5.5 seconds idle
before each connection, received all frames and passed the same stream check.
With only three samples per path its medians were 208.46 ms built-in and
221.99 ms plugin. This is an exploratory device-startup check, too small to
establish a reliable latency difference; it excludes ticket HTTP and browser
decode just like the warm run.

The page is about 112 KB uncompressed. The prototype bundles a copy of the
approximately 5 MB host executable and starts another capture process. This is
deliberately easy to package; it is not a measurement of the minimum possible
screen-provider binary or memory footprint.

## Costs still unresolved

- **Windows service/session launch is the largest architectural gap.** The
  existing service has a privileged console-session supervisor specifically
  for screen. An ordinary plugin runs in session 0. Supporting the plugin here
  needs an authorized session-aware launch contract, including session changes,
  pipe ownership, and cleanup. This spike does not generalize that launcher.
- The screen UI shares source with the normal client. This proves the page
  boundary, not independent screen-package versioning or an external SDK.
- Cold capture/device setup needs a larger controlled measurement; the small
  cold check above is not a release-performance gate.
- Manual focus, touch, fullscreen, clipboard, monitor switching, disconnect
  input release, and login/lock/UAC behavior still need platform verification.
- Hosted cross-origin framing and a physical two-machine route remain checks.

## Recommendation

Keep screen built in for the plugin-foundation release. The ordinary plugin
transport and iframe are viable enough to keep experimenting with; there is
no measured warm-transport reason to force a rewrite. Extracting the privileged
service launcher and independently versioning the UI need a separate decision.
If those costs do not buy reuse, drop the experiment and retain the built-in
screen. Other apps can still use the plugin system.
