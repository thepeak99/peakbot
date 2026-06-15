# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- Fixed compacted conversations breaking on `/load` (#59). Context compaction now persists the per-message `compacted` flag and the inserted summary, so a conversation compacted in a previous session reloads with its compacted state intact instead of resurrecting the full history and forcing manual re-compaction.
