# Provider pages in frames

A provider can have a page of its own, written with anything a browser runs. The machine serves the page, and the client shows it in a frame. Released in 0.3.0, as contract version 1.

This started as a spike, and records it. The follow-up hardening and current
release gates are in [plugin-release.md](plugin-release.md), including
contract versioning, bounded sockets, linked-path checks, CLI/package tests,
and automatic frame-ticket recovery after a host restart.

## How it works

1. **The provider names a folder** in what it says of itself: `{"page": "C:\\…\\page"}`. The descriptor (`/.well-known/peek`) doesn't show the path, only `"frame": true`.
2. **Peek serves that folder** at `/p/<name>/` on the machine. Only plain file names are allowed, and nothing outside the folder is served. The page may be framed only by the hosted client and by the machine's own pages (`frame-ancestors`).
3. **The client asks for a ticket**: `POST /api/frame/<name>`, with its token. A ticket works for that one provider only and lasts 12 hours. The token never enters the frame.
4. **The client opens a frame** at `https://<machine>/p/<name>/#ticket=…`:
   - sandbox: `allow-scripts allow-same-origin allow-forms`;
   - no top navigation, no pop-ups, no dialogs;
   - `fullscreen` and `clipboard-write` are handed over on purpose;
   - the box always names the provider and the machine.
5. **The page opens its own socket** to its provider: `/api/frame/<name>/ws?ticket=…`, from its own origin only.
   - Text and binary pass both ways, as they are.
   - The provider sees the channel open with `{"from":"page"}`.
   - On a view-only host, `do` and `peer` are refused, and binary is refused too.
6. **The client sends its colours** once the frame loads (`{"t":"theme","vars":{…}}`), only to the machine's origin.

## Where a page shows

- **In place of the screen,** at `/screen/<machine>/<provider>` on the hosted client.
  - A tab bar has Screen and each app with a page; the toolbar gets a button for each app too.
  - Back, bookmarks and reloads work.
  - The machine stops sending the screen while a page is shown.
- **In a panel over the screen,** on the client a machine serves itself, opened from a toolbar button for each app.
- **The Plugins page is reached from Settings** (Manage plugins) on both clients, not from the toolbar.

## Downloads

A provider can hand peek a file. On the pipe, `OFFER` (kind 8) is sent on the page's channel with `{"path": …, "name": …, "ref": …}`.

1. Peek makes a link that works once, within a minute.
2. Peek sends the link on that channel: `{"t":"file","ref":…,"url":"/api/frame/<name>/file/<id>","name":…,"size":…}`.
3. The page opens the link. Peek streams the file as a download (`Content-Disposition`, with the name in UTF-8), and the browser's download manager takes it from there.

The frame needs `allow-downloads` for this. Which files a provider may offer is its own business: it runs as peek's user, and could send the bytes itself.

`apps/files` is a file explorer built into peek, off by default. Turn it on and choose folders on the Plugins page. It is also buildable alone as an example plugin in its own Rust crate (with only `serde_json`, nothing of peek's):
- the machine's owner names the shared folders in its settings;
- the browser can't add a folder;
- paths are checked after links and junctions are followed;
- hidden files are skipped: names starting with a dot, Windows' own files, and anything marked hidden or system;
- it's read-only.

## Plugins

A plugin is a folder with a `peek-plugin.json`:

```json
{
  "name": "files",
  "version": "1.0.0",
  "description": "Browse the folders you share, and download their files.",
  "run": ["bin/peek-files"],
  "settings": { "folders": { "label": "Shared folders", "kind": "folders", "default": [] } }
}
```

- **Where it lives:** `plugins/<name>/` in peek's config folder.
- **How it starts:** `run` is started in that folder. Its first word is a program on the machine (`node`), or one the plugin brings (`bin/peek-files`, which is `bin\peek-files.exe` on Windows).
- **What it's written in:** anything that reads and writes its stdin and stdout. The examples are one of each: `files` in Rust (a 213 KB program, which ran in about 4 MB, 0.6 MB of it its own), `frame` (notes) in Node (an idle Node is about 35 MB).
- **What isn't copied:** a folder's `.git`, and `target/`, where building a Rust plugin leaves its output.
- **Its data:** `PEEK_DATA_DIR` names a separate folder for persistent data.
- **Default state:** `on_by_default` is an optional boolean, true by default.
- **Its version:** `version` is an optional string shown when inspecting a plugin.
- **Its settings** (kinds `folders`, `text`, `switch`) reach it as JSON in `PEEK_SETTINGS`.
- **What it needs:** peek checks `needs` and says what's missing. It doesn't bundle a runtime.

### Managing them

The **Plugins** page is peek's own page, served by peek itself at `/p/plugins/`, and opened from Settings → Manage plugins. It lists:
- peek's own providers;
- commands added with `peek provider add`;
- plugins.

For each it shows whether it runs, how often it has started, how it last ended, its recent log, and what it runs. You can turn each on and off and restart it, and for plugins also change settings and remove. None of this restarts peek.

### Installing

From a folder on the machine, an https git address, or an npm package:
- `peek plugin add <source>` on the machine;
- or the Plugins page, which first fetches the plugin to look at, then shows exactly what it runs (with peek's user's full rights), what it needs, and whether it brings npm dependencies (installed without their install scripts).

Peek can't hold a plugin to what it says it does; the page says so.

`peek plugin update <name> [source]` updates an installed plugin, using its recorded source if none is given. Settings that remain valid, its on/off state and its data are kept. Removing a plugin keeps its data; `peek plugin remove <name> --purge` deletes it, including data kept after an earlier removal.

### Who may change what

- **Any paired browser:** can look.
- **A view-only machine:** refuses everything else.
- **Remote install** (`peek plugin remote-install on|off`): when off, a browser can only turn plugins on and off and restart them. Installing, removing and settings are refused.
- **Never possible:** turning off the screen, or removing peek's own providers.
- **npm names and git addresses** are checked before anything runs; nothing goes through a shell's parsing.

### The client

A page can tell the client the machine's providers changed (`postMessage({t:"changed"})`). The client listens only to the machine's own origin, and re-reads the providers. That's how a plugin's tab appears as soon as it's installed.

## Why the sandbox can be loose

The page runs as the machine's origin, never as the hosted client's. So the browser keeps it away from the client's page and storage, which hold the token for every machine.

`allow-scripts` with `allow-same-origin` is only an escape when the frame shares its parent's origin, and here it doesn't. What the page can reach on the machine, its provider already has: the provider runs as peek's user and can read peek's token file.

## Checked in a browser (Chrome)

Setup:
- the hosted client, built, and served with the site's own headers (port rule widened to reach a test copy on :10000);
- a test copy of peek on this PC through Tailscale Serve;
- `examples/provider/frame`.

Results:
- **What the page could do:**
  - draw a live chart from binary samples (32 floats at 30 Hz, about 4 KB/s);
  - take typed notes, which the provider kept;
  - use its own storage;
  - use WebGPU.
- **What was blocked:**
  - reading the client's storage, page or address (SecurityError);
  - navigating the client's tab (blocked by the sandbox);
  - opening a window (blocked by the sandbox).
- **Refused by the server:** no token, another site's origin, paths that leave the folder, a provider with no page.
- **A fault found and fixed:** focus moving into the frame paused the screen as if the window had gone to the background.
- **The file explorer, as its own page:**
  - it browsed two shared folders;
  - a non-ASCII file name (`Résumé – final.txt`) downloaded under its own name;
  - a 1 GB file downloaded through the browser and Tailscale Serve in about 6 s on this PC, with peek steady at about 32 MB.
- **Refused, driving the provider directly:**
  - `..`;
  - a junction that leads out of a share (not listed either);
  - backslashes;
  - absolute paths;
  - a hidden file;
  - an unknown share;
  - a folder asked for as a file;
  - a link used a second time;
  - another provider's ticket;
  - the right ticket from another origin.

- **files, again, rewritten in Rust** (test copy, driven as its page does):
  - installed from its folder over the Plugins page's socket, with nothing needed on the machine and its `target/` left behind;
  - every refusal above held again, and a file marked hidden by Windows was left out (the Node version didn't look at attributes);
  - the non-ASCII download and a single-use link, as before; 64 MB downloaded whole;
  - its page, unchanged, worked against it;
  - about 4 MB working set and 0.6 MB private, against about 35 MB for an idle Node.
- **Plugins, in the browser** (built hosted client, test copy through Serve):
  - files installed from a folder and running, without restarting peek;
  - its shared folder set in its settings (a folder that doesn't exist was refused), and files restarted sharing it;
  - notes installed, with its tab appearing at once;
  - two client faults found and fixed: a tab that didn't appear after a restart, and the frame reloading whenever the machine's saved data changed.
- **Plugins, over the page's socket:**
  - on, off and restart;
  - the screen refused as a thing to turn off;
  - removal, which deletes the folder;
  - a view-only machine refusing all changes;
  - with remote install off, install, remove and settings refused while on and off still worked;
  - bad sources refused before anything ran: an npm name with `&`, http, a folder with no manifest, a made-up id;
  - a real GitHub repository cloned and refused for having no manifest;
  - a missing npm package refused with npm's own 404.

## Open questions

- **The page's files aren't behind the ticket.** Anyone who can reach the machine can fetch them, like peek's own page. Its data is behind the ticket.
- **The origin is shared.** The page shares an origin with the client the machine serves itself, which keeps that machine's token. The provider has that token anyway; a separate origin would need another Serve port.
- **Port 443 only.** The site's policy allows Tailscale names on port 443 only, for frames as for everything else.
- **Mobile Safari and Firefox are untested.** So is Chrome's local network prompt from inside a frame.
- **No frame on a machine's card yet.**
- **Pausing the screen on a page wasn't seen working.** The test tab was in the background, so the screen read 0 fps whatever was shown.
- **Not tried across two machines.** All of it ran on one PC: a test copy of peek, reached through Tailscale Serve.
- **The explorer can't search** beyond the folder it shows, and has no previews.
- **`peek plugin` commands weren't run here.** On a machine with the service installed they act on the service's config folder, which needs an administrator. They're covered by unit tests through the same code.
- **No automatic updates.** Peek does not check for newer versions.
- **No real install from npm or git.** No plugin is published yet.
- **Under the service, `node` must be on the machine's PATH,** not just the user's: a Node from a per-user installer like scoop won't be found by SYSTEM. A plugin that brings its own program doesn't have this problem.
- **A plugin that brings a program needs one for each OS and processor.** A folder holds the one for its machine; how a git repository or an npm package would carry several (npm's per-platform packages, as esbuild does, say) isn't decided.
- **Tickets don't survive a restart of peek.** An open page reconnects and gets refused until the client opens it again.
- **A page's socket can't tell a refused ticket from a dropped connection** in every client: Bun saw 1006, not 4403.
