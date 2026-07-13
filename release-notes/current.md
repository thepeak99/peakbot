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
- Web UI: the message composer now rests at a single row and grows line-by-line
  with its content up to a cap (past which it scrolls) — reclaims the vertical
  space the previous multi-row resting height ate, especially on mobile.
- Web UI: slimmer, subtler scrollbars tuned to the zinc theme (translucent
  grey thumb that lifts on hover).
- Web UI: the foreground `bash` output moved out of the fixed strip above the
  composer into its own **Bash** drawer tab — expand it when you want, and it
  no longer eats vertical space or gets pushed off by new messages (#121). The
  output is still a scrollable buffer that auto-follows the tail and pauses
  following when you scroll up to read history; a handle badge marks a running
  command. The backend mirrors up to 500 tail lines to the panel; the terminal
  UI still clips to its fixed 5-row height.
- Web UI: the drawer's tab handle rail no longer overlaps transcript text or
  the composer's Send button on mobile/tablet — the conversation column reserves
  the rail's width, while the message input spans the full width. A small gap
  now sits between the content and the rail so the transcript scrollbar reads as
  separate from the tabs.
- Web UI: tapping/clicking outside the open drawer closes it (in addition to
  Escape) on mobile/tablet only — on desktop the drawer stays put as a
  persistent side rail, so a stray click in the conversation won't dismiss it.
- Web UI: browser notification when a task finishes while the tab is not
  focused — opt-in per session via a bell toggle in the top bar (#119).
  Enabling it now fires a one-off confirmation notification so you get instant
  feedback that it works (real pings only fire when the tab is backgrounded).
- Web UI: the **Files** tab now shows a per-file kind badge (created /
  modified / read) and a "Copy for git add" button that copies the changed
  paths, newline-joined, for staging (#126).
- Web UI: a "⏳ N queued" counter in the top bar shows how many messages are
  queued while the agent is busy (#123, counter only — per-message deletion
  still pending a queue refactor).
- Web UI: a **＋** button next to the conversations chip opens a fresh
  conversation in a new browser tab (new tab with no `?convo=` mints a new
  server session; same origin so the auth cookie carries). Present on both
  desktop (top bar) and mobile (bottom bar).
- Web UI: light/dark theme toggle in the top bar (☀/🌙, next to the notify
  bell). Dark remains the default; light mirrors the same semantic zinc ramp
  by remapping the zinc CSS variables under an `html.light` class, so every
  panel, border, and text tone flips with zero per-component changes. The
  choice persists in `localStorage` and falls back to your OS
  `prefers-color-scheme` on first visit; an inline head script applies it
  before first paint to avoid a flash. Fenced code blocks stay dark in both
  themes by design.
- Web UI: mobile layout fixes (#117) — full-width root to avoid horizontal
  overflow, the drawer no longer overlaps the mobile bottom bar, header/footer
  heights aligned with the drawer, iOS safe-area padding on the bottom bar,
  and the cwd trigger truncates instead of pushing the bar off-screen.
- Web UI: the cwd picker dropdown now opens upward on tablets too (640–1023px),
  not just phones — previously it pinned downward because the `top-full mt-1`
  base class overrode the `dropUp` prop on intermediate widths. Pattern now
  matches `ModelSwitcher` and `ConversationsPicker`.
