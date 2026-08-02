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

- **`/setup` is now a real first-run wizard.** The ten steps (welcome,
  locations, provider, models, persona, services, access, start on boot,
  multi-agent, review) now drive a dedicated `/api/setup` REST surface on
  the server: `GET /api/setup` reports wizard state, `POST /api/setup/config`
  validates the draft through the same writer the CLI uses (so the validation
  rules the wizard documents are literally the rules the server enforces),
  `POST /api/setup/install` runs any auto-installable components the draft
  requested (MCP servers, custom skills), and `GET|POST|DELETE /api/setup/service`
  registers or unregisters the start-on-boot service. The previous
  preview chips are gone; buttons that mutate config call the API, the
  Review page renders the exact `config.yaml` that would be written, and
  the reload matrix on the review page still tells you what needs a
  restart versus what applies on the next `/new`. Leaving `/setup`
  still starts over.

- **`/setup` install flow moved to the Review step.** The Locations step
  used to carry a real Install button; it now just shows the machine facts
  and the PATH state verdict (the `PATH state` label also got a `block`
  so it no longer renders jammed against its value). The Review step has
  a single `Install` button that writes the config (`POST /api/setup/config`,
  which validates server-side and is skipped if it fails) and then runs
  the binary install (`POST /api/setup/install`), with progressive labels
  (`Writing config…` then `Installing…`). The success panel combines both
  results — config path + backup, installed target/action, and the
  post-install PATH verdict/notes — and a `Cancel` ghost link sits beside
  the button. If the config write succeeded but the install failed, the
  error panel says the config was written so the user can retry without
  losing it.

- **`peakbot install` and the `service` subcommand: the new end-user
  onboarding verbs.** `peakbot install` copies the binary onto the user's
  `PATH` (system or user install, detected from the running privileges)
  and prints next-step hints; on Linux it drops an XDG-friendly layout,
  on macOS `/usr/local/bin` (or `/opt/homebrew/bin` on Apple Silicon
  when writable), and on Windows it uses `%LOCALAPPDATA%\Programs`. The
  `service` verb owns start-at-boot: `peakbot service install` registers
  a per-user Linux **systemd** unit (no root required, `$XDG_CONFIG_HOME/systemd/user/peakbot.service`),
  a per-user **launchd** plist on macOS (`~/Library/LaunchAgents/com.ai-bots.peakbot.plist`),
  or a per-user **Task Scheduler** task on Windows; `uninstall` and
  `status` round out the trio. The platform files are rendered from a
  shared template (`src/install/`) so behaviour stays in lockstep across
  OSes, and the start-on-boot step in `/setup` now writes through the
  same code path rather than a UI-only affordance.

- **First-run bootstrap: open `/setup` automatically.** PeakBot now refuses
  to start when it cannot find a `config.yaml` (the old message about an
  empty config was quietly confusing). On a desktop session with a usable
  `Browser::open`, the server boots anyway, binds its web port, and opens
  the user's default browser at `/setup`. On a headless box — no
  `DISPLAY`, no `WAYLAND_DISPLAY`, SSH with no forwarded browser — the
  binary exits with a single-line hint pointing at `peakbot install`
  and `peakbot --tui --provider …` so the user is never trapped behind
  a screen they cannot see.

- **`--web` is now a deprecated alias; web is the default.** Running
  `peakbot` with no UI flag starts the web server (same `peakbot --web`
  used to do). `peakbot --tui` is the terminal; the REPL/CI path is
  unchanged. The new default matches what 99% of users actually want,
  while `--web` keeps working with a one-time deprecation log so existing
  scripts and muscle memory keep functioning.

- **Persona is now a config key, live-reloadable.** The hardcoded
  `prompt_persona()` helper is replaced by a `config.persona` string (the
  "Spirit" box in `/setup`). The first message of the system prompt
  becomes `You are <name>. <persona>` instead of the previous hardcoded
  "You are PeakBot…" line, and changing `config.persona` followed by
  `/new` (no restart) re-words the agent on the next turn. Existing
  configs without `persona` fall back to the previous default wording —
  no migration step.

- **Web-token file: a stable, leaked-by-design cookie replacement.** The
  bearer token PeakBot uses for its WS / REST auth is now materialised to
  `~/.peakbot/web-token` with mode `0600` on POSIX (ACL-restricted on
  Windows). The file's mtime is the issue time, and the token rotates on
  `peakbot service restart` (the next start generates and writes a fresh
  one). The first-run wizard reads the file to auto-fill the browser's
  auth prompt, so the user pastes nothing.

- **Friendly `AddrInUse` error.** When the web bind fails — usually
  because another instance is already serving — PeakBot now prints
  which PID is holding the port (via `/proc/net/tcp` on Linux, `lsof`
  on macOS, `netstat` on Windows), which `peakbot install`/`service`
  state owns it, and the exact `lsof -nP -iTCP:<port> -sTCP:LISTEN`
  command for the operator to verify. The previous one-line "address
  in use" gave no path forward.

- **`/setup` UI went from preview to wired.** The wizard's React side
  was already structurally correct; this PR rewires it from preview chips
  to real API calls (`web/src/setup/api.ts`), adds `web/src/setup/catalog.ts`
  for the model/provider enumeration that drives the dropdowns, removes
  `fixtures.ts` (it was a no-network stand-in), and threads the new
  validation messages (e.g. duplicate alias, missing `default_model`)
  back into the relevant step. The Start-on-boot step in particular
  now reflects the actual platform service state (systemd unit
  installed? launchd plist present?) rather than a checkbox the user
  could tick and forget.
- **Light mode: fenced code blocks are readable again.** A fence with no language tag (```` ``` ````) never gets highlight.js's `.hljs` class, so it had no colour of its own and inherited the bubble's text colour — which light mode remaps to near-black, on the deliberately dark code-block background. `.markdown-body pre` now sets github-dark's own foreground explicitly, so labeled and unlabeled fences match in both themes. The light-mode inline-code chip is also scoped to `:not(pre) > code`; unscoped it outranked `.markdown-body pre code` and smudged its background into fenced blocks. Fenced code stays dark in both themes — that part was always intentional.
- **The web UI now says that your messages always go to the orchestrator.** Watching a sub-agent only filters the transcript; it cannot re-route input, because `delegate` is a tool the orchestrator calls. Nothing said so, so "watching architect" read as "talking to architect". While a role is selected, the composer carries a one-line notice (with a *Clear* link back to the global view) and the placeholder softens to "Message the orchestrator…". The Agents panel and the TUI's `/subagents` output state the same rule, so it's learnable from wherever you are.
- **PR 3**: Client-side search in the conversations dropdown — filter by name/model (case-insensitive substring) or conversation id (prefix match), with keyboard navigation (↑/↓/Enter/Esc) and stable ordinals.
- **The Session tab lists agents as cards rather than a table.** Five columns squeezed into a narrow drawer left the model alias and the token counts fighting for width. Each lane is now its own card — the agent and its model on top, `in` / `out` / `calls` / `msgs` in a row beneath — which reads without horizontal scanning and grows better as roles are added. The message count comes from the same derivation the Agents panel uses, so a lane reads identically in both places.
- **The Agents panel now shows API calls next to its message counts.** A role's badge read `204 msg` while the Session tab said `68 calls` for the same agent, which looked like the two panels disagreeing. They didn't: one counts transcript messages, the other counts requests to the model, and a single call typically produces two or three messages (the reply, a tool call, its result). Both numbers now sit side by side — `204 msg · 68 calls` — reading from the same lane stats the Session tab uses, with a tooltip that states the relationship.
- **The web UI is now branded Shifu.** The tab title, the top bar, the welcome banner, agent message labels and desktop notifications all read *Shifu*, and the browser tab carries the new sage mark (favicon plus an apple-touch icon for home-screen installs). The top bar's `✦` placeholder is the mark itself now. This is a presentation change only: on-disk directories, environment variables, the session cookie, the crate, and the binary all keep the `peakbot` name, so no existing configuration or conversation history moves.

- **Fixed: the top bar's dropdowns opened but could not be clicked.** The header uses `backdrop-blur`, and a backdrop filter creates a stacking context even on an unpositioned element — which quietly trapped the conversation, model, and folder panels inside the header's own layer. Once the transcript grew a positioned wrapper, that wrapper (later in the DOM, no background of its own) painted on top of the whole header: the panels stayed visible but every click landed behind them. The header now ranks itself explicitly rather than relying on document order.

- **One less full pass over the transcript per frame.** The message list was being filtered twice per render with identical arguments, once for the transcript and once for the todo and file panels. They share a single pass now — most noticeable in long conversations, where that array is large.

- **Delegate tool gains a required `parent_task_id` parameter.** Orchestrators must add a todo item before delegating and pass its id — the web UI now renders each sub-agent's todos nested under that parent task in the Todo panel (one level deep). Unparented or old transcripts fall back to the previous flat per-lane grouping. REPL unchanged.

- **Delegate salvage file.** When a `delegate` call completes, the sub-agent's last ≤10 earlier assistant messages are saved to a plain-text file in the temp directory, and a pointer is appended to the delegate result so the orchestrator can `file_read` it. Useful when the sub-agent's final reply is terse but the real report lives in an earlier message. Write failures degrade silently; hookless Ollama sub-agents get no file.
