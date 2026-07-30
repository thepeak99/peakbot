# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Agents panel: per-delegation drill-down.** Roles are now grouped — one row
  per agent showing its delegation count and total messages, with an expandable
  list of individual delegations beneath. Click a role to watch all its turns,
  or a `call N` to scope the transcript to that single delegation. Delegation
  boundaries are derived from contiguous runs of a role's turns rather than the
  orchestrator's `delegate` ToolCall, so splitting still works in long
  conversations where that call is no longer in the transcript.

- **Session tab: per-agent token breakdown.** When sub-agents are enabled, the
  Session panel now lists every lane (orchestrator + each role) with the model
  it runs on and its cumulative `in` / `out` / `calls`. Per-lane input/output
  tokens now accumulate across the session instead of holding only the last
  request's count; `/stats`'s by-lane table gained the same two columns. The
  flat totals and the compaction gate are unchanged.
