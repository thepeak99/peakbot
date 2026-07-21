# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- Sub-agents (multi-agent pipeline) are now **opt-in per conversation and off by
  default**. Configuring a `pipeline:` block makes sub-agents *available* but no
  longer force-enables them. Enable them per conversation via the web **Agents**
  panel checkbox or the new **`/subagents on|off`** terminal command. The choice
  is only changeable before the first message and locks once the conversation
  starts; it's persisted so resumed conversations keep their setting. The web
  "Enable subagents" checkbox is now a real toggle instead of a grayed-out
  mirror of the config.
