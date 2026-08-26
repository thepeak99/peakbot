# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes
- Added the agent's current phase to the web UI's "working…" indicator — the top bar now reads `working… · Compacting memory.md...` during compaction instead of an unlabelled spinner that looked hung. Same `status_message` the TUI banner has always shown (compaction, tool names, retries), now rendered in the browser; no backend or protocol change.
