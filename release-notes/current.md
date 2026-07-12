# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- Web UI: replaced the static right-rail sidebar (plus its separate mobile
  hamburger drawer) with a single `TabbedDrawer` — vertical tab handles pinned
  to the right edge that slide a shared drawer body in/out. One responsive
  mechanism for desktop and mobile. Tabs: **Session** (stats + context),
  **Todo**, **Files**, **Tasks** (background processes).
- Web UI: added a **Files** tab listing the files the agent touched this
  session (create/edit), derived live from the transcript's file-edit tool
  calls (#126, list-only).
- Web UI: tab handles now show a pointer cursor on hover (desktop a11y).
- Web UI: the message composer is now taller by default and auto-grows with
  its content — a roomy resting height (larger on desktop, smaller on mobile),
  expanding line-by-line up to a cap, past which it scrolls internally.
- Web UI: slimmer, subtler scrollbars tuned to the zinc theme (translucent
  grey thumb that lifts on hover).
- Web UI: the foreground `bash` panel output is now a scrollable buffer that
  auto-follows the tail and pauses following when you scroll up to read
  history (#121). The backend now mirrors up to 500 tail lines to the panel;
  the terminal UI still clips to its fixed 5-row height.
- Web UI: browser notification when a task finishes while the tab is not
  focused — opt-in per session via a bell toggle in the top bar (#119).
- Web UI: the **Files** tab now shows a per-file kind badge (created /
  modified / read) and a "Copy for git add" button that copies the changed
  paths, newline-joined, for staging (#126).
- Web UI: a "⏳ N queued" counter in the top bar shows how many messages are
  queued while the agent is busy (#123, counter only — per-message deletion
  still pending a queue refactor).
- Web UI: mobile layout fixes (#117) — full-width root to avoid horizontal
  overflow, the drawer no longer overlaps the mobile bottom bar, header/footer
  heights aligned with the drawer, iOS safe-area padding on the bottom bar,
  and the cwd trigger truncates instead of pushing the bar off-screen.