# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Fix:** Preserve reasoning/thinking blocks on the Anthropic provider: thinking blocks (with signatures) are now captured, persisted in conversations, and replayed on the wire (thinking-first ordering within assistant messages), fixing HTTP 400s in Claude tool loops and satisfying Kimi/MiniMax reasoning-history requirements. New per-model config knobs `preserve_reasoning` (default true) and `display_reasoning` (default false, thinking hidden from UI). Non-Anthropic providers and knob-off models strip reasoning at capture.
- **Fix:** Orchestrator context meter in the web Session panel / TUI status bar now correctly survives save/load after delegations — it no longer shows a sub-agent's context size.


