# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes
- **Fix: memory.md auto-compaction silently never fired outside the startup directory** — the gate resolved `memory.md` against the process cwd instead of the session cwd, so with the web server started outside a project dir (the normal pipeline-mode setup) it read the wrong file and skipped. The path now joins onto `StateManager::session_cwd()`, the block is extracted into a testable helper with a regression test, and an oversized memory.md without a compaction model now logs a warning instead of skipping silently.
- The web UI's cwd picker dropdown now has a "Recent" quick-jump section: the directories you worked in most recently, derived from your conversation history. Shown newest first, capped at 8 entries, with dead (no-longer-existing) directories filtered out — the whole section hides when there's nothing to show. Picking a recent directory goes through the same confirm guard as browsing.
