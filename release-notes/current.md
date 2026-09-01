# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes
- Added config **profiles**: a named, boot-selected overlay (`profiles:` in the master config) that replaces `tools:` and/or `memory:` wholesale for a deployment. Select one with `--profile <name>`; it is applied last (after per-repo merge) and re-applied on every reload, so it survives `/cd`, `/model`, `/new`, `/load`, and `/pipeline` — a profile that disables `bash` stays disabled even if a repo you `/cd` into tries to re-enable it. Profiles are master-config only; a `profiles:` block in a per-repo `.peakbot/config.yaml` is ignored with a boot warning. See `docs/configuration.md`.
