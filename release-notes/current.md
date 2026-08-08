# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Multi-pipeline — named teams with per-conversation selection

**BREAKING: legacy `pipeline:` config block no longer boots.** PeakBot now refuses to start if the old `pipeline:` block (with `enabled:` and `orchestrator_prompt:`) is present — including `enabled: false`. A hard boot error prints a migration hint.

**Migration recipe:** wrap your existing `agents:` under a named `pipelines:` entry, move `orchestrator_prompt:` to `orchestrator.prompt`, and drop `enabled:`:

```yaml
# Before (legacy — no longer boots)
pipeline:
  enabled: true
  orchestrator_prompt: "You lead the team."
  agents:
    reviewer:
      model: sonnet
      prompt: "You review diffs."

# After (new shape)
pipelines:
  - name: my-team
    orchestrator:
      prompt: "You lead the team."
    agents:
      reviewer:
        model: sonnet
        prompt: "You review diffs."
```

**What's new:**

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

- **Conversation saves now keep memory use constant.** File storage streams
  conversation JSON and its summary index through a fixed 256 KiB buffer instead
  of building the entire document in memory before every write. Save errors are
  surfaced before the atomic rename, existing JSON formatting stays byte-for-byte
  compatible, and large conversations no longer trigger ever-growing allocator
  retention during persistence.

- **WebSocket and stdio outbound paths are now bounded.** A half-open peer
  (laptop sleeps, no FIN/RST) used to let the per-socket outbound channel
  accumulate undelivered `state` snapshots without limit — the bug took a
  production machine to ~17 GB RSS in two hours. The outbound plumbing is
  now split by delivery contract (`src/ui/outbound.rs`): a bounded FIFO of
  32 for ordered control frames, and a 1-deep coalescing slot for `state`
  snapshots, so per-socket memory is bounded regardless of peer behaviour.
  The web writer gains a 120 s write timeout (tear-down, never retry — a
  timed-out `send` may have written a partial frame into the TLS stream)
  and a 30 s keepalive ping so the timeout can observe a dead idle peer.
  The forwarder's `select!` now also arms on writer completion so a
  torn-down socket actually detaches from the session registry instead
  of pinning `attached > 0` against the idle-TTL reaper. The stdio
  transport gets the same shared channel with no timers (NDJSON has no
  half-open pipe; a slow consumer is legitimate backpressure). The
  `state` frame on the wire is byte-identical — no frontend change.

- **Background processes are shared session state.** The `bash_bg` lifecycle text no longer claims a process dies when the conversation ends — the registry is session-wide, so a sub-agent's processes outlive its delegation. Sub-agents now receive a snapshot of what is already running in their preamble, and the delegate result tells the orchestrator what the delegation left running or stopped. Both renderings are silent when there is nothing to report.

- **`pipelines:` list** — declare one or more named teams. Each entry has `name`, a required `orchestrator:` (model + optional prompt/persona), and an `agents:` map of sub-agent roles. Declaration order is UI order.
- **Orchestrator as team member** — the orchestrator is configured alongside sub-agents inside each pipeline entry. `orchestrator.model` sets the orchestrator's model (falls back to `default_model`), `orchestrator.prompt` adds an addendum to the orchestrator recipe, and `orchestrator.persona` replaces the global persona for that pipeline's orchestrator only.
- **Per-conversation pipeline selection** — pick a team with `/pipeline <name>` (TUI) or the pipeline selector in the Agents tab (web). Locked after the first turn. `/pipeline none` = single agent mode. `/new` keeps the current selection.
- **Spaced pipeline names** — names may contain spaces (`^[A-Za-z0-9_ .-]+$`, trimmed, control characters rejected). `/pipeline` takes the rest of the line, so `/pipeline Generic Dev Team` selects that team. The Agents tab renders each selector row on two lines — the full name, then `🎬 <model> · N sub-agents` — so long names are no longer truncated.
- **Locked orchestrator model** — when a pipeline is selected, the orchestrator's model is derived from the pipeline config. `/model <alias>` refuses with an explanatory message; the top model selector is read-only.
- **`/subagents` removed** — the command is replaced by `/pipeline`. A tombstone message points to the new command.
- **Setup wizard emits the new shape** — the Multi-agent step now writes a `pipelines:` entry (name, orchestrator model/prompt/persona, member roles) instead of the legacy block, and the config endpoint validates it with the same checks the binary runs at boot. A config imported with several `pipelines:` entries is shown read-only and written back verbatim — the wizard never rewrites your teams.
- **Orchestrator-scoped context meter** — the Session tab context meter now shows orchestrator context only (sub-agent turns don't move it), labelled "Orchestrator context". This is the same signal the compaction gate reads.
