# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- Added a `memory:` `enabled` switch that governs the whole memory.md feature: `memory.enabled: false` stops injecting the memory.md instructions into the system prompt **and** skips auto-compaction. (Default on.)
- Added a `tools:` config block to filter built-in tools — `disabled:` (blocklist) or `only:` (allowlist), mutually exclusive, validated at load. Unknown tool names and setting both lists are rejected. Reload-safe (applied on the next session verb).
- Dropped the injected `thought` field from the `todo` tool (now registered ungated, like `think`): the task text already carries the plan, and some models (e.g. MiniMax) structurally refuse the field, which tripped a "thought missing" nudge on every todo call.
- Sub-agents now inherit a lean shared context in their preamble: the live environment block (cwd, time, OS, shell) plus a per-role-filtered skills list. They deliberately do **not** get the crusader persona, the core tool-usage guidance, the memory.md workflow, or `agents.md` — a sub-agent's `prompt` is its whole persona, and everything else goes in the delegated task.
- Added per-role skill gating under `pipeline.agents.<role>.skills` — mirrors the `tools:` filter with `only:` (allowlist) XOR `disabled:` (blocklist), plus an `enabled:` master switch (`false` gives a role no skills). Skill names are validated at boot against the discovered skills.
- Added `pipeline.orchestrator_prompt` — extra framing appended to the **orchestrator's** system prompt when sub-agents are active. In sub-agents mode the orchestrator also **drops the crusader persona** (it confuses an agent whose job is to coordinate a team) while keeping the core tool guidance; agentless mode is unchanged. The persona/orchestrator framing is recomputed at the single agent-rebuild seam, so toggling sub-agents flips it correctly.
