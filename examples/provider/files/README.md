# Files: a native Peek plugin

This example has its own program and page, with no dependency on Peek's Rust
code. It exposes only the shared folders configured by the machine's owner.

Build from this directory, then place the binary where the manifest expects:

```powershell
cargo build --release
New-Item -ItemType Directory -Force bin
Copy-Item target/release/peek-files.exe bin/peek-files.exe
peek plugin add .
```

On Linux, copy `target/release/peek-files` to `bin/peek-files` instead,
preserving executable permissions. Each package needs a binary for the host's
OS and architecture. No Node runtime is required.

In Peek's Plugins page, set Files' shared folders and save. Open Files from
the Pages section. It supports browsing and one-time downloads; it is read-only.

The manifest and protocol contract are in [plugin-release.md](../../../docs/plugin-release.md).
