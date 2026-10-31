# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Features

- **Per-profile tab colour.** Each profile now has a colour. Set it with `ui.color` (one of red, orange, yellow, green, teal, blue, purple, pink). If unset, a stable colour is derived from the profile name.
- Conversation tabs show a coloured top accent. The selection chip and conversation picker rows show a colour dot.
- Conversation list entries now carry their profile. The conversation index is rebuilt once on upgrade.
