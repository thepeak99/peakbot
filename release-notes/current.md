# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Fix:** reopening a conversation whose pipeline is declared in that directory's `.peakbot/config.yaml` no longer drops the pipeline with 'no longer configured' — the resume path (`create_session`) now re-reads the conversation's per-repo config and rebuilds the pipeline set before validating the saved selection (falls back to the boot set on any failure).