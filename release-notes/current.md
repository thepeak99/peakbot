# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- docs: add mandatory "Worktree Workflow" to `agents.md` — every task gets its own worktree, implement → watch CI → merge only if told to (else a background watcher waits for a human merge) → delete the worktree/branch only once merged.
