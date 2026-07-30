# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Agents panel: per-delegation drill-down.** Roles are now grouped — one row
  per agent showing its delegation count and total messages, with an expandable
  list of individual delegations beneath. Click a role to watch all its turns,
  or a `call N` to scope the transcript to that single delegation. Delegation
  boundaries are derived from contiguous runs of a role's turns rather than the
  orchestrator's `delegate` ToolCall, so splitting still works in long
  conversations where that call is no longer in the transcript.

- **Session tab: per-agent token breakdown.** When sub-agents are enabled, the
  Session panel now lists every lane (orchestrator + each role) with the model
  it runs on and its cumulative `in` / `out` / `calls`. Per-lane input/output
  tokens now accumulate across the session instead of holding only the last
  request's count; `/stats`'s by-lane table gained the same two columns. The
  flat totals and the compaction gate are unchanged.

- **Light mode: fenced code blocks are readable again.** A fence with no language tag (```` ``` ````) never gets highlight.js's `.hljs` class, so it had no colour of its own and inherited the bubble's text colour — which light mode remaps to near-black, on the deliberately dark code-block background. `.markdown-body pre` now sets github-dark's own foreground explicitly, so labeled and unlabeled fences match in both themes. The light-mode inline-code chip is also scoped to `:not(pre) > code`; unscoped it outranked `.markdown-body pre code` and smudged its background into fenced blocks. Fenced code stays dark in both themes — that part was always intentional.
- **The web UI now says that your messages always go to the orchestrator.** Watching a sub-agent only filters the transcript; it cannot re-route input, because `delegate` is a tool the orchestrator calls. Nothing said so, so "watching architect" read as "talking to architect". While a role is selected, the composer carries a one-line notice (with a *Clear* link back to the global view) and the placeholder softens to "Message the orchestrator…". The Agents panel and the TUI's `/subagents` output state the same rule, so it's learnable from wherever you are.
- **PR 3**: Client-side search in the conversations dropdown — filter by name/model (case-insensitive substring) or conversation id (prefix match), with keyboard navigation (↑/↓/Enter/Esc) and stable ordinals.
