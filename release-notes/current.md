# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Fixes

- The "Orchestrator context" meter now stays at 0 after a compaction. Before, it could jump back to the pre-compaction size when the conversation was reloaded, after `/new` or `/reset`, or when a late usage report for a request sent before the compaction arrived. A just-compacted conversation no longer triggers another compaction for these reasons.
- Web focus mode no longer hides the last answer while a slash command such as `/compact` runs. The spinner and Stop button behave as before. Wire change: the state frame carries a new `command_running` flag; the protocol version is unchanged because the field is additive.
