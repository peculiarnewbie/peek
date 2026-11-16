# Processes: a light task manager for Peek

What is running on a machine, live: processor and memory for each process,
sorting, filtering, grouping by name, and ending one. Built into Peek, off by
default; turn it on in the Plugins page. Like `apps/files`, it has nothing of
Peek's in it and also builds alone as a plugin:

```
cargo build --release
mkdir -p bin && cp target/release/peek-processes bin/     (Windows: peek-processes.exe)
peek plugin add .
```

- **Nothing is read while nobody looks.** The page asks for updates while it
  is visible, once a second, and gets only what changed. The card on the
  machine list is empty unless a program is busy or memory is nearly full.
- **Ending a process** names it by its number and when it started, so a
  number that has gone to another process is not ended. Peek itself, the
  machine's first process, kernel threads, and processes Windows cannot run
  without are never ended. The system's and other users' processes only with
  the "Allow ending the system's and other users' processes" setting. A
  view-only host refuses it.
- **Linux** reads `/proc`; End sends SIGTERM, Force SIGKILL, through a pidfd.
- **Windows** reads every process in one `NtQuerySystemInformation` call;
  Memory is the private working set, as Task Manager shows. End is
  TerminateProcess. The Windows half compiles but has not been run yet.

The messages are described at the top of `src/lib.rs`.
