# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- Fixed (regression in 0.24.0): `/cd` (terminal, web folder picker, stdio) switches to the target folder's agent again. Entering a folder with a `.peakbot/config.yaml` starts the new conversation on that repo's first profile and agent, unless you already use one of its profiles. Leaving it returns to your default profile. Between folders without repo profiles your chosen profile is kept. The model follows the new agent, subagent model overrides reset when the agent changes, and an `ℹ Switched to … for …` line tells you when it happens.
