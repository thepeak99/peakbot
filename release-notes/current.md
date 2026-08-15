# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Fix:** Anthropic HTTP 400 on the second user message of extended-thinking conversations ("`thinking` or `redacted_thinking` blocks in the latest assistant message cannot be modified"): reasoning blocks are now replayed grouped by the model response that generated them, so each assistant message on the wire carries only the thinking its own response produced. Conversations saved before this fix heal automatically on load.
