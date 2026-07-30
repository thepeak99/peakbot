# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Delegate tool gains a required `parent_task_id` parameter.** Orchestrators must add a todo item before delegating and pass its id — the web UI now renders each sub-agent's todos nested under that parent task in the Todo panel (one level deep). Unparented or old transcripts fall back to the previous flat per-lane grouping. REPL unchanged.
