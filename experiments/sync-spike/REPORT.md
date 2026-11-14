# One-way sync spike

## What was built, and how to run it

This experiment uses two local Peek hosts, each with an independently configured plugin, and a Bun relay joining their page sockets. The source indexes and hashes files on demand; the destination writes part files, verifies them and renames them into place. The relay preserves message order in each direction. The source sends an `end` message after its final metadata check, so the destination waits for that check before renaming. The test hosts run on the same Windows machine, on loopback. This is not a network or two-physical-machine measurement.

From the repository root in PowerShell:

```
cargo test -j 4 --manifest-path experiments/sync-spike/Cargo.toml
cargo build -j 4 --release
cargo build -j 4 --release --manifest-path experiments/sync-spike/Cargo.toml
bun experiments/sync-spike/bench.ts
```

The benchmark creates a temporary package and two configuration folders, and removes the entire temporary tree after stopping the hosts. It uses 2,023 files and 626,901,158 file bytes initially. The 2,000 small files have deterministic sizes between 1,024 and 3,023 bytes; medium and large files have random contents. The benchmark uses a four-chunk window by default. Times below include both index passes, including hashing, unless noted. MB means 1,000,000 bytes.

## Results

### Baseline

The 536,870,912-byte file copied on the same disk in 0.154 seconds, or about 3,486 MB/s. This very short copy can reflect caching or filesystem copy acceleration and is not a physical disk throughput measurement.

### Initial copy

The transfer copied 2,023 files and 626,901,158 payload bytes in 23.08 seconds, about 27.2 MB/s. Both index passes completed without errors, and SHA-256 matched for every source file at the destination. The relay forwarded 6,407 messages A to B and 6,409 B to A. Its A-to-B binary count was 627,122,497 bytes including chunk headers. Peak working sets reported by Windows were 15,409,152 and 17,162,240 bytes for the two main hosts, and 11,161,600 and 8,601,600 bytes for their sync plugins. Additional Peek child processes also ran. The measurement includes process startup before the copy.

### Nothing to do

The next connection copied zero files, skipped all 2,023, and transferred zero binary bytes. Two index passes and candidate hashes took 8.68 seconds. The relay forwarded 6 messages A to B and 8 B to A.

### Relay drop

The relay closed after forwarding 214,998,650 binary bytes including headers. A new relay copied the remaining 321,912,832 payload bytes, reported one resumed file, and produced a matching 536,870,912-byte file. The resumed connection took 9.99 seconds. The exact part length can differ from bytes forwarded because closing can leave messages in flight.

### Destination host restart

The destination host was stopped after approximately 189.3 MB of binary messages. Its restarted instance retained its configuration and part file. The next connection reported one resumed file, copied 348,389,376 payload bytes in 10.29 seconds, and produced a matching big file. The interrupted socket reported 1006 with an empty reason on both sides.

### Changed during read

Seven bytes were appended to the source big file during its transfer. The first pass transferred 536,870,912 bytes but copied no file; the next index pass copied the new 536,870,919-byte version. The two passes took 12.71 seconds and transferred 1,073,741,831 payload bytes. The final hashes matched. The benchmark polled the destination path and obtained one hash while it existed; that hash matched the final source. This sampling does not prove what happened between polls.

### Slow link and the eight-message queue

The relay delayed each forwarded message by 20 ms in each direction. With window 4, the big file completed in 51.71 seconds, about 10.3 MB/s of binary messages. With window 16, it completed in 51.71 seconds, also about 10.4 MB/s. With window 64, the source socket closed after 262,186 binary bytes were forwarded: code 1013, reason `slow page; reconnect`; the other socket reported 1006 with an empty reason. That run took 2.32 seconds, or 0.11 MB/s of binary messages before closure. These numbers include indexing and both index passes. The relay reads sockets eagerly and delays forwarding in its own queue, so this is not a faithful simulation of a receiver that stops reading the socket. Even so, a source burst at window 64 overflowed the host queue.

Every normal benchmark shutdown also produced 1006 with an empty reason in Bun after the relay initiated close. Those codes cannot be treated as proof that the host independently failed. The 1013 and its reason were explicitly delivered by the host.

### Destination-only file

The destination-only file still contained `keep` after the trials.

## What the receiving side needed from the host

The provider sees an open channel with `from: page`; it does not learn the other host's identity. The relay has to know both addresses and both unrestricted machine tokens, request a provider-scoped frame ticket from each with the right Origin, and open both page sockets. The provider must declare a page even though the relay does not render it. The relay joins the channels; neither provider can reach the other through the host by itself.

The pipe has a 256 MiB payload limit in `src/provider.rs`. The 256 KiB chunks and roughly 291 KB index messages passed in these runs; a larger index was not tested. The pipe limit is not a demonstrated end-to-end WebSocket limit.

The host sends WebSocket pings every 5 seconds and closes after 15 seconds without a read, according to `src/server.rs`. The benchmark did not isolate an idle keepalive test. One long-lived management socket stopped answering after the earlier trials; obtaining a fresh ticket and socket before each settings change worked, but the precise reason for the earlier timeout was not established. Plugin restart closes its page channel, and host restart drops sockets and invalidates old tickets. The restarted host needed a fresh ticket and a new relay connection.

## Bulk data over existing channels

The initial transfer's 27.2 MB/s was about 0.78% of the 3,486 MB/s same-disk copy measurement, subject to that baseline's caching or acceleration. The sync time includes reading and hashing the source twice, hashing destination candidates on the second pass, pipe and socket transport, relay forwarding, writing, and durability flushes. These runs did not isolate those costs. The unchanged run's 8.68 seconds shows that indexing and candidate hashing alone are substantial for this data set, but is not a direct subtraction from the initial run.

Successful runs verified file hashes, and the receiver checks chunk offsets before appending. No data corruption or reordering was observed in those runs. The 1013 window-64 result shows an explicit queue overflow, rather than a successful large-window transfer. The relay's eager socket reads limit what the delay test says about network backpressure.

## What resuming took

The receiver retained a part file on the destination volume and requested its length on a fresh channel. It synced part data after each 8 MiB and again before rename, checked the final hash, and only then published the file. Both a relay interruption and host restart resumed the large file and produced the expected hash. No host-side progress or durable channel state was used. The relay had to obtain fresh tickets and reconnect; neither host restarted the transfer itself. Abrupt failure between durability flushes and actual power loss were not tested.

## Surprises and dead ends

The first run failed with `Access is denied. (os error 5)` on every attempted finish: on Windows, opening a part file read-only before `sync_data` did not provide the access needed for the flush. Opening it for writing fixed that run. A first benchmark run also timed out waiting for a reused management socket; reconnecting it allowed the rest of the scenarios to run. The normal Bun socket close observations were 1006 with empty reasons, including successful runs. Hash checks and the index rescan cost enough to dominate some short transfers. The test uses same-machine loopback and cannot measure tailnet latency, actual network failures, or different disks.

The spike skips dot-prefixed names and symbolic-link entries while walking. It accepts only simple relative slash-separated paths, uses a decimal nanosecond timestamp string, clamps the configured window to at least one, and handles one file per channel at a time. It reports counts cumulatively across the two index passes. The benchmark appends seven fixed bytes to trigger the change test, stops the destination after about 180 MiB, and uses 205 MiB as its relay-drop threshold. The relay applies delay serially within each direction and counts complete binary messages including their headers. These are experiment choices, not host guarantees.

There are untested edges: the watcher samples rather than continuously checks the destination path; the slow-link relay buffers eagerly; there is no independent idle or plugin-restart timing test; and the program does not establish safe behavior for arbitrary destination links, source truncation during a read, or power loss. No claim is made that this spike is a complete sync implementation.
