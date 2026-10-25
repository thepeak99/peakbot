# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Fixes

- Conversation list no longer hides older conversations after upgrading: the summary index could be marked up-to-date while missing pre-existing conversations. Affected installs self-heal on the next list — the old index format is discarded and rebuilt from the conversation files.
