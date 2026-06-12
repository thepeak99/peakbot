# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Added Anthropic prompt caching** (`prompt_caching:` in the Anthropic
  provider config). Injects ephemeral `cache_control` breakpoints to cut
  input-token cost on the stable request prefix. Modes: `auto` (default —
  top-level breakpoint the API advances as the conversation grows, 5-minute
  TTL; recommended for multi-turn Claude usage), `auto_1h` (auto with a
  1-hour TTL), `manual` (system prompt + last tool + last message — the
  LiteLLM `system/0` + `user/-1` injection-point shape, 5-minute TTL), and
  `off` (no caching; set explicitly when pointing `base_url` at a local
  llama-server endpoint that may not honor `cache_control`). Configurable in
  both the legacy `provider:` block and per-model in the multi-model
  `providers:` format.
- **Fixed the token/context counter under-reporting on Anthropic with
  prompt caching.** Anthropic reports per-request `input_tokens` as the
  *uncached* portion only (cached tokens are reported separately), so with
  caching on (the `auto` default) the status-bar "Context %" and the
  session input-token total read far too low — and, more dangerously,
  automatic context compaction could fail to trigger because the tracked
  context size never approached the window. The session hook now derives a
  provider-agnostic input size as `total_tokens − output_tokens` (falling
  back to the raw `input_tokens` when a provider leaves `total_tokens`
  unset). This is correct for both usage shapes — Anthropic, where cached
  tokens are *additive* to `input_tokens`, and OpenAI/OpenRouter, where
  they're already *folded into* `input_tokens` — with no double-counting.
- **Added a `view_image` tool so the agent can SEE local images it finds
  during a task** — screenshots, diagrams, UI captures, charts. Until now
  only the user could attach images (via `[img:…]`); the agent had no way
  to pull an image into its own vision context mid-task. `view_image`
  takes a filesystem path (`/abs`, `./rel`, `~/home`), reuses the existing
  image loader (10 MB cap, PNG/JPEG/GIF/WEBP allowlist), and returns the
  image as a structured tool result that rig feeds into the model's sight.
- **Fixed image tool results being flattened to text on the way back to
  the model.** PeakBot rebuilds its chat history into wire messages every
  turn, and the tool-result reconstruction *always* wrapped the stored
  result as `ToolResultContent::Text` — so an image tool result (e.g. from
  `view_image`) was replayed to the model as a giant base64 *string*, not
  an image block. The model saw no picture. All four reconstruction sites
  (`get_agent_history`, `build_current_turn_message`, the resumption
  builder, and `convert_conversation_to_rig_messages`) now use rig's own
  `ToolResultContent::from_tool_output`, which parses image-JSON results
  into `Image` and leaves plain results as text. Verified end-to-end
  against a live Anthropic-Messages gateway.
- **Added a first-class `anthropic` provider with a custom `base_url`.**
  This is the enabler for `view_image`: in rig 0.36 the Anthropic Messages
  API is the *only* provider whose tool-result channel actually delivers
  images to the model (OpenRouter substitutes a placeholder string, Ollama
  drops the image, OpenAI errors). Pointing `base_url` at a local
  llama-server's `/v1/messages` endpoint lets a local multimodal model
  receive images from `view_image`. `view_image` is therefore registered
  only on the Anthropic provider; other providers don't advertise a tool
  that would be a silent no-op for them. Config:

  ```yaml
  provider:
    type: anthropic
    config:
      base_url: http://localhost:8080   # local llama-server, or omit for Claude
      model: your-multimodal-model
  ```

- **Upgraded `rig-core` 0.36 → 0.38.2, fixing image tool-results being
  rejected by the Anthropic Messages API.** rig 0.36 serialized an image
  tool-result as a newtype enum variant, which collided the inner and
  outer serde tags and emitted a *duplicate* `type` key
  (`{"type":"image","type":"base64",…}`). The Anthropic API (and any
  Messages-compatible proxy, e.g. LiteLLM) reads the last key, sees
  `base64`, and rejects the request with
  `Input tag 'base64' … does not match any of the expected tags`. So
  `view_image` could never actually deliver an image to a real Anthropic
  endpoint. The fix shipped upstream in rig 0.38 (the variant now nests
  the source: `{"type":"image","source":{…}}`). A regression test in
  `view_image.rs` converts a generic image tool-result through rig's own
  Anthropic conversion and pins the wire shape so this can't regress
  silently.

- **Fixed `[img:…]` inline attachments being rejected on the Anthropic
  provider for local/gateway models.** The inline image path gated on a
  conservative model-name match (`gpt-4o`, `claude-3`, …), so attaching an
  image to a multimodal model whose name we don't recognise — a local GGUF,
  or a gateway model like `minimax/MiniMax-M3` — failed with *"Model … does
  not support vision"*, even though the `view_image` tool worked fine on the
  same model. `view_image` gates on the *transport* (Anthropic carries
  images natively), not the model name; the inline path now does the same.
  On the Anthropic provider, `[img:…]` is allowed regardless of model name —
  a genuinely non-vision model just replies "I can't see images" rather than
  being blocked pre-flight. All other providers keep name-based detection.

- **Added a per-model `vision:` config flag to force image support on or off.**
  Set it on any model in the `providers:` list (or the legacy `provider:`
  block) to override auto-detection:

  ```yaml
  providers:
    - name: local
      type: anthropic
      base_url: http://localhost:8080
      models:
        - name: my-multimodal-gguf
          alias: local
          vision: true        # force image support on
  ```

  `vision: true` enables `[img:…]` user attachments on any provider, and —
  on the Anthropic provider — also registers the `view_image` tool so the
  model can load images itself. `vision: false` forces image support off
  (e.g. to stop a name-detected vision model from accepting images, saving
  tokens). Omitting `vision:` keeps the existing auto-detection. Note:
  `view_image` only works on the Anthropic transport (the lone provider whose
  tool-result channel delivers images), so `vision: true` on a non-Anthropic
  model enables `[img:…]` but does not register `view_image`.


- **Replaced the background-process "3-turns-and-stop" circuit breaker
  with a per-process cooldown the model controls.** `bash_bg start` now
  takes an optional `cooldown_secs` (default **60**): after a process
  injects a `[bg output]` turn, its further output is coalesced and
  flushed in one batch once the cooldown elapses, so a chatty log no
  longer wakes you on every line. `cooldown_secs: 0` is real-time (inject
  every batch) — use it for external-input bridges (telegram, webhooks,
  IRC) where you react to each line immediately. Process exits always
  bypass the cooldown, and a real user message flushes all buffered
  output at once. A quiet buffer that fell silent mid-window still
  flushes on time via a deadline-driven wakeup in the agent loop.
- **Removed the `treat_as_user_input` flag and the two-tier (capped /
  unlimited) model.** The single `cooldown_secs` knob now covers what the
  tier flag used to: set `0` for instant external input, leave the
  default for ambient feeds. Background turns render uniformly (🛰
  Background). The `telegram-chat` skill now starts its listener with
  `cooldown_secs: 0`. Conversations saved before this change still load
  (the stale `any_unlimited` field is ignored).


- **Added a "Comment Style" rule to `agents.md`** and trimmed the
  branch's own comments to match: keep comments to 2–3 lines (ideally 1),
  explain *why* not *what*, never narrate plans/stages/temporal changes,
  and fix stale/bloated comments on sight. Condensed the over-long doc and
  inline comments around `snap_boundary_past_tool_results` and its tests
  to follow the new rule.

- **Fixed a compaction bug that could crash the next request with an
  Anthropic "orphaned tool_use" error.** When the compaction boundary
  happened to land *on* a `tool_result` whose `tool_use` was just before
  it, the tool call was correctly preserved but the inserted conversation
  *summary* got wedged between the `tool_use` and its `tool_result`.
  Anthropic requires the `tool_result` to immediately follow the
  `tool_use`, so the very next request failed with
  `tool_use ids were found without tool_result blocks immediately after`.
  The compaction boundary is now snapped forward past any trailing
  `tool_result`(s) so a `tool_use`/`tool_result` pair is never split by
  the summary — applied both where the plan is built and, defensively,
  where it is applied.

- **Fixed a false-negative in the `release-tag` Make target's
  protected-branch path.** When `master` is protected, `release-tag`
  opens and merges a `release/<v>` PR via the Gitea API. It previously
  decided merge success by looking for a top-level `.sha` in the
  `POST /pulls/N/merge` response — but Gitea returns an empty body on a
  successful merge, so the target aborted (`exit 1`) *even though the PR
  had merged*, leaving the tag unpushed and the release half-done. The
  check now queries the authoritative source — `GET /pulls/N` and tests
  `.merged == true` — so a genuine merge is recognised and the release
  proceeds to push the tag and build/publish. The merge response is
  still printed for diagnostics if the merge actually failed.

- **Refreshed the `chat_welcome` REPL snapshot for v0.7.0.** The 0.7.0
  release bump updated `Cargo.toml` but not the insta snapshot that
  renders the welcome banner (`PeakBot v…`), leaving `master` with a
  failing `cargo test`. The snapshot now reflects `v0.7.0`.
