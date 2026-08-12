# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Virtualized chat transcript for long conversations (#277).** The web SPA used to mount every message in the DOM and re-parse all markdown on every websocket `state` frame — 5k-message conversations froze the page. The chat list now uses `@tanstack/react-virtual` and mounts only the visible window (overscan 3) with dynamic measurement, end-anchored with auto-follow on append. `<Message>` is now `React.memo` with a value comparator (fresh `JSON.parse` identities every frame made identity comparison useless), and `useTranscriptScroll` was slimmed to pin/unread bookkeeping only — scrolling is the virtualizer's job. *Known tradeoffs:* scroll jumps are instant, not smooth (smooth defeats end-anchoring); browser text selection and `Ctrl+F` cover only the mounted window (a few viewports). The full-snapshot wire protocol is the next perf wall at 5k messages and is left out of scope to be measured separately.
