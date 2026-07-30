# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Light mode: fenced code blocks are readable again.** A fence with no language tag (```` ``` ````) never gets highlight.js's `.hljs` class, so it had no colour of its own and inherited the bubble's text colour — which light mode remaps to near-black, on the deliberately dark code-block background. `.markdown-body pre` now sets github-dark's own foreground explicitly, so labeled and unlabeled fences match in both themes. The light-mode inline-code chip is also scoped to `:not(pre) > code`; unscoped it outranked `.markdown-body pre code` and smudged its background into fenced blocks. Fenced code stays dark in both themes — that part was always intentional.
- **The web UI now says that your messages always go to the orchestrator.** Watching a sub-agent only filters the transcript; it cannot re-route input, because `delegate` is a tool the orchestrator calls. Nothing said so, so "watching architect" read as "talking to architect". While a role is selected, the composer carries a one-line notice (with a *Clear* link back to the global view) and the placeholder softens to "Message the orchestrator…". The Agents panel and the TUI's `/subagents` output state the same rule, so it's learnable from wherever you are.
