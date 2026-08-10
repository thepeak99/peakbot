# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes
- **Stop now stops everything (#183).** Pressing Stop (web), Esc (TUI) or `/stop` cancels the
  running turn immediately instead of at the next model round-trip: the foreground `bash`
  child is killed, any running delegation is torn down mid-tool along with its shell child,
  and every `bash_bg` process is stopped (the `🛰 N bg` counter drops to zero). The chat log
  reports what died — `Agent stopped by user (killed 1 bash process, 2 background processes)`.
  Stop while idle remains a no-op. *Known limitation:* on Windows only the direct child is
  terminated, so a grandchild spawned by the shell may survive.
- **Fixed `/load` dropping delegate tool pairs (#271).** Resumed conversations no longer lose all record of their delegations when sub-agent turns separate the delegate call and result.
