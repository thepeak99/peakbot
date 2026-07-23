# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- Added a `memory:` `enabled` switch that governs the whole memory.md feature: `memory.enabled: false` stops injecting the memory.md instructions into the system prompt **and** skips auto-compaction. (Default on.)
- Added a `tools:` config block to filter built-in tools — `disabled:` (blocklist) or `only:` (allowlist), mutually exclusive, validated at load. Unknown tool names and setting both lists are rejected. Reload-safe (applied on the next session verb).
