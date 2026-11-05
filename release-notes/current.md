# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Fixes

- Stop now interrupts memory.md compaction and in-conversation context compaction: a double Stop press drops the in-flight summarization and ends the turn as Stopped, leaving memory.md and the history untouched. A cancelled memory compaction retries on the next message.
- Desktop (Linux): pasting an image (Ctrl+V) into the chat box now attaches it. The bundled WebKitGTK did not pass clipboard images to the page, so the app now reads the image from the clipboard itself. Text paste, the 📎 button, drag-and-drop and macOS/Windows/browser behaviour are unchanged.
- `/stats` no longer double-counts API calls and per-agent token totals for the main agent.
