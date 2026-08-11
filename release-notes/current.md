# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes
- **Stop now stops the running turn immediately (#183).** Pressing Stop (web), Esc (TUI) or
  `/stop` cancels the running turn instead of at the next model round-trip: the foreground `bash`
  child is killed via the per-turn cancellation token, and any in-flight sub-agent is torn down
  mid-tool along with its shell child. Background (`bash_bg`) processes are deliberately spared —
  they survive Stop and are only killed by the rebuild paths (`/new`, `/model`, `/load`, `/cd`,
  shutdown). Stop while idle remains a no-op. The chat log reports what died: `Agent stopped by
  user (killed 1 bash process)` when a foreground shell was running, otherwise the bare
  `Agent stopped by user`. *Known limitation:* on Windows only the direct child is terminated,
  so a grandchild spawned by the shell may survive.
- **Fixed `/load` dropping delegate tool pairs (#271).** Resumed conversations no longer lose all record of their delegations when sub-agent turns separate the delegate call and result.
