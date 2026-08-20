# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Fix:** concurrent conversation saves no longer clobber each other: `FileStorage::save` now uses a per-conversation temp file `.tmp.<id>.json` instead of a shared `.tmp.json`, fixing data loss when multiple conversations are saved simultaneously (memory optimization F0/T1).
- **Fix:** reopening a conversation whose pipeline is declared in that directory's `.peakbot/config.yaml` no longer drops the pipeline with 'no longer configured' — the resume path (`create_session`) now re-reads the conversation's per-repo config and rebuilds the pipeline set before validating the saved selection (falls back to the boot set on any failure).
- **Fix:** `list_directory` tool now returns a placeholder message `"(empty directory)"` instead of an empty string when listing empty directories, preventing crashes in LLM providers that reject empty tool results.
- **Internal:** added `image_cache`, a content-addressed spill cache that writes image bytes to disk once (keyed by sha256) and hands back a small `ImageRef`, so large base64 payloads stop living in the conversation transcript.
- **Internal:** `view_image` now spills its loaded bytes into `image_cache` and includes an `image_ref` (id + display name) in its output alongside the unchanged full-payload response, plus a helper to extract that ref from a serialized tool output without touching the base64 data — groundwork for eliding old image payloads from the transcript.
- **Internal:** `ChatMessage` gained an `images` field so a `view_image` result renders as a single `🖼 <name>` line instead of a raw base64 dump, and `elide_binary_payload` can now drop a stale image's wire payload from the transcript while keeping the row's displayable image ref intact.