# For coding agents

peek is a remote desktop host for LAN and Tailscale: a Rust host, and a
browser client it serves. README.md says what it does; this says how to work
on it.

## The parts

- `src/`: the host. `main.rs` (flags and commands), `server.rs` (HTTP,
  sockets, who may connect), `engine.rs` and `screen.rs` (capture, diff,
  encode), `webp.rs`, `platform/` (Windows and Linux), `pairing.rs`
  (pairing codes, tokens, Tailscale Serve), `install.rs` (`peek install`,
  `peek uninstall`), `provider.rs` and `plugins.rs` (programs started beside
  peek, and plugins), `vitals.rs`, `power.rs`.
- `apps/files`, `apps/processes`: apps that come with peek, each also a
  crate of its own.
- `client/`: the browser client (Solid 2, Vite). `client/src/` is shared;
  `client/machine/` is the page a machine serves itself, which `build.rs`
  bundles into the host; `client/site/` is the hosted site
  (TanStack Start), with its docs and changelog pages in
  `client/site/pages/`, and `client/hosted/` its static files.
- `docs/`: the provider, page and plugin contracts. `bench/`: measurements.
  `examples/provider/`: providers to copy. `experiments/`: opt-in spikes.

## Build, check, test

```
cd client && bun run check                    # TypeScript
cargo build --release                         # also builds the client, with Bun (or npm)
cargo test
cargo check --target x86_64-pc-windows-gnu    # the Windows half, from Linux
```

The hosted site's dev server: `cd client && bun run dev:hosted --port 5199`.

Click to first frame under 500 ms is a hard limit, then bandwidth, host CPU
and memory, responsiveness: nothing may slow the path to the first frame or
add work per frame. A change a user will notice gets a line in
`client/site/pages/changelog.html`.

## Running peek for a test

A peek may already be installed and running on this machine, on port 9182.
Leave it alone: do not stop it, send it input, or point commands at it.

- Give a test peek a config folder and a port of its own:

  ```
  PEEK_CONFIG_DIR=$(mktemp -d) target/release/peek --view-only --bind 127.0.0.1 --port 9320
  ```

  `PEEK_CONFIG_DIR` keeps its token, pairing data and plugins apart from
  the installed peek's. Commands for it take the same folder and port:
  `PEEK_CONFIG_DIR=… target/release/peek --port 9320 plugin list`.
- Never run `peek install`, `peek uninstall` or `peek pair` for real.
  `install` and `uninstall` change the real autostart (systemd user unit,
  or Windows logon task, service and firewall rule) whatever
  `PEEK_CONFIG_DIR` says, and `pair` changes Tailscale Serve. To test them,
  put fake `systemctl` and `tailscale` scripts first on `PATH`, and point
  `PEEK_CONFIG_DIR`, `HOME`, `XDG_CONFIG_HOME` and `XDG_DATA_HOME` at a
  temporary folder.
- Stop what you start by its PID (`ss -ltnp` finds it from the port), not
  with `pkill -f`, and remove the temporary folders.
