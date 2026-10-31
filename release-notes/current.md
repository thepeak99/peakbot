# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Features

- Click any image in the chat — agent replies and composer previews included — to view it full screen (✕ or Esc to close); images in agent replies no longer open a new tab / system browser on desktop.
- Conversation tabs now shrink like a browser's: they start wide and shrink evenly to fit, the + button always follows the last tab, scroll arrows (‹ ›) appear only when the tabs overflow, and closing tabs with the mouse keeps the ✕ under the cursor.
- **Per-profile tab colour.** Each profile now has a colour. Set it with `ui.color` (one of red, orange, yellow, green, teal, blue, purple, pink). If unset, a stable colour is derived from the profile name.
- Conversation tabs show a coloured top accent. The selection chip and conversation picker rows show a colour dot.
- Conversation list entries now carry their profile. The conversation index is rebuilt once on upgrade.
