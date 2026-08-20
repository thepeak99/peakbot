# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Fix:** concurrent conversation saves no longer clobber each other: `FileStorage::save` now uses a per-conversation temp file `.tmp.<id>.json` instead of a shared `.tmp.json`, fixing data loss when multiple conversations are saved simultaneously (memory optimization F0/T1).
- **Fix:** reopening a conversation whose pipeline is declared in that directory's `.peakbot/config.yaml` no longer drops the pipeline with 'no longer configured' — the resume path (`create_session`) now re-reads the conversation's per-repo config and rebuilds the pipeline set before validating the saved selection (falls back to the boot set on any failure).
- **Fix:** `list_directory` tool now returns a placeholder message `"(empty directory)"` instead of an empty string when listing empty directories, preventing crashes in LLM providers that reject empty tool results.