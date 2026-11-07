# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Fixes

- Desktop: "Check for updates…" (macOS app menu and tray) now only checks — it no longer downloads and installs an already-known update immediately. When an update is found you get a dialog, and installing happens from the update banner.

- **Mandatory Discord release announcement.** Every release (stable and RC) now gets exactly one Discord embed-card post as the final step, only after the release is verified (all 10 Gitea assets, shifu `SHA256SUMS`, GitHub `latest` tag for stables). One format: an embed card that always pings @everyone via `content` + `allowed_mentions` (a mention inside an embed does not ping), posted to the `DISCORD_WEBHOOK_AI` webhook with `?wait=true`. Full format, template, and limits are defined in `agents.md` › "Release announcement (mandatory)".
