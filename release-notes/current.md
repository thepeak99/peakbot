# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Fixed**: Stop cropping the `thought` parameter in tool call UI display (closes #22). The `thought` field on all tools (not just `think`) is now rendered in full instead of being truncated to 100 characters, so the agent's reasoning is fully visible in the chat transcript.
- **Fixed**: Preserved whitespace in message rendering (issue #5). Leading and inner whitespace in agent replies is now preserved, fixing YAML and code block rendering.
- **Added**: Regression test for whitespace preservation using YAML as the canonical test case.
- **Fixed**: Sanitize orphaned tool calls/results when loading conversations and after context compaction (closes #12, #15). A single boundary repair (`sanitize_tool_pairs`) drops any `ToolCall` without a matching adjacent `ToolResult` (and vice-versa) before the messages reach the wire layer, eliminating a class of "the model crashes on resume" errors caused by truncated or hand-edited conversation files.

- **Added**: Auto-generated conversation titles (closes #16). After the first assistant response, PeakBot calls the LLM to generate a short, descriptive title (max 60 chars) stored in `Conversation.title` and shown in the `/conversations` listing. Falls back to the timestamp-based creation name until a title is generated. Title generation is fire-and-forget — failures are logged but don't interrupt the conversation.
- **Added**: Automatic memory.md compaction. At the start of each conversation, if `memory.md` exceeds the configured threshold (default: 50KB), the compaction model rewrites it — condensing old episodic entries into procedural/semantic knowledge, merging redundant sections, and preserving structure. A system message notifies the user when compaction occurs. Configurable via `memory.enabled` and `memory.threshold_bytes` in `config.yaml`.

<!-- Add your changes below. Examples:
- Added new feature X
- Fixed bug Y
- Updated documentation
- Breaking change: removed deprecated API Z
-->
