# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Refactor (#11, slice 1):** Extracted shared PTY-spawning core into new `src/pty_runner.rs` module — `LineBuffer`, `PtyStatus`, `PtyHandle`, `spawn`, `strip_ansi`, `truncate_line`. `bg_processes.rs` now delegates spawn ceremony, reader thread, and kill to `pty_runner`, keeping only registry-level concerns (multi-process map, two-tier circuit breaker, drain semantics). Zero behaviour change — sets up shared use by the upcoming `bash` PTY port and live-output panel.
