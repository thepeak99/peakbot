# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Refactor (#11, slice 1):** Extracted shared PTY-spawning core into new `src/pty_runner.rs` module — `LineBuffer`, `PtyStatus`, `PtyHandle`, `spawn`, `strip_ansi`, `truncate_line`. `bg_processes.rs` now delegates spawn ceremony, reader thread, and kill to `pty_runner`, keeping only registry-level concerns (multi-process map, two-tier circuit breaker, drain semantics). Zero behaviour change — sets up shared use by the upcoming `bash` PTY port and live-output panel.
- **Feature (#11, slice 2):** UI scaffolding for the foreground `bash` tool panel. New `BashPanelState` enum on `AppState` (`Idle` / `Running` / `Finished`) with four `StateManager` setters (`start_bash_panel`, `update_bash_panel_tail`, `finish_bash_panel`, `clear_bash_panel`) and a new `src/ui/repl/bash_panel.rs` renderer (bottom-strip layout, 5-line scrolling tail, header glyph + pid/exit + elapsed/duration, inline `stdin» _` row when Running). Panel is hidden when `Idle` and lives between chat and input when active. No producer wiring yet — slice 3 ports the foreground `bash` tool to `pty_runner` and starts feeding this state. 5 snapshot tests pin the visual contract for each lifecycle state.
