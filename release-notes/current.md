# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Fixed**: Stop cropping the `thought` parameter in tool call UI display (closes #22). The `thought` field on all tools (not just `think`) is now rendered in full instead of being truncated to 100 characters, so the agent's reasoning is fully visible in the chat transcript.
- **Fixed**: Preserved whitespace in message rendering (issue #5). Leading and inner whitespace in agent replies is now preserved, fixing YAML and code block rendering.
- **Added**: Regression test for whitespace preservation using YAML as the canonical test case.
- **Fixed**: Sanitize orphaned tool calls/results when loading conversations and after context compaction (closes #12, #15). A single boundary repair (`sanitize_tool_pairs`) drops any `ToolCall` without a matching adjacent `ToolResult` (and vice-versa) before the messages reach the wire layer, eliminating a class of "the model crashes on resume" errors caused by truncated or hand-edited conversation files.
- **Added**: Auto-generated conversation titles (closes #16). After the first assistant response, PeakBot calls the LLM to generate a short, descriptive title (max 60 chars) stored in `Conversation.title` and shown in the `/conversations` listing. Falls back to the timestamp-based creation name until a title is generated. Title generation is fire-and-forget — failures are logged but don't interrupt the conversation.
- **Added**: Automatic memory.md compaction. At the start of each conversation, if `memory.md` exceeds the configured threshold (default: 50KB), the compaction model rewrites it — condensing old episodic entries into procedural/semantic knowledge, merging redundant sections, and preserving structure. A system message notifies the user when compaction occurs. Configurable via `memory.enabled` and `memory.threshold_bytes` in `config.yaml`.
- **Added**: `think` tool now shows "🤔 Thinking..." in the UI instead of displaying the full verbose reasoning text, keeping the chat transcript clean.
- **Added**: `bash_bg` tool — long-running PTY-backed background processes. Four verbs (`start`, `stop`, `list`, `send_line`) on a single tool with an `action` discriminator. Output streams into a per-process ring buffer (default 200 lines) and reaches the LLM between turns as synthetic user messages framed `[bg output]`. Tool count: 10 → 11. See `bash-background.md` for the design.
- **Added**: Two-tier circuit breaker for background output. Capped tier (default) — log feeds, build watchers — caps consecutive auto-injection at 3 turns without a real user input. Unlimited tier (`treat_as_user_input: true`) — telegram bridges, webhooks, IRC bots — bypasses the cap and resets the counter on each contribution, functionally equivalent to a typed user message.
- **Added**: `MessageSource` discriminator on `ChatMessage`. Distinguishes human-typed turns from background-process-driven synthetic turns; persisted on disk (pre-v6 files default cleanly to `Human` via `#[serde(default)]`). Renderer styles bg rows with 🛰 (capped) or 💬 (unlimited) so the user can scan the transcript at a glance.
- **Added**: `🛰 N bg` segment in the working title when at least one background process is running. Sits alongside `⏳ N queued`.
- **Added**: `/bg` slash command — human-readable listing of background processes (id, pid, tier, status, buffer fill, command).
- **Added**: `portable-pty = "0.9"` dependency for cross-platform PTY backing on Linux/macOS/Windows.
- **Changed**: `/new`, `/model`, and `/load` kill all background processes — they are scoped to the conversation they were spawned in. Buffers and live processes do not survive a restart (in-memory registry only).
- **Fixed**: `bash_bg` now uses the same environment variables as the synchronous `bash` tool. Processes spawned via `bash_bg start` inherit the custom `env` map from the `bash:` config section, overriding inherited OS variables — matching the `bash` tool's behavior.




