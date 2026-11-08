# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Fixes

- Web UI: images now display on remote profiles, including `view_image` tool results, which previously showed only a `🖼 alt` text stub. Uploaded-file thumbnails that fall back to the served file now use the remote profile's route too.

- Desktop: "Check for updates…" (macOS app menu and tray) now only checks — it no longer downloads and installs an already-known update immediately. When an update is found you get a dialog, and installing happens from the update banner.

- **Mandatory Discord release announcement.** Every release (stable and RC) now gets exactly one Discord embed-card post as the final step, only after the release is verified (all 10 Gitea assets, shifu `SHA256SUMS`, GitHub `latest` tag for stables). One format: an embed card that always pings @everyone via `content` + `allowed_mentions` (a mention inside an embed does not ping), posted to the `DISCORD_WEBHOOK_AI` webhook with `?wait=true`. Full format, template, and limits are defined in `agents.md` › "Release announcement (mandatory)".

- Web UI: changing directory (picker or typed `/cd <path>`), typing `/new`, or `/load <id>` while a turn is running no longer gets silently queued until the turn ends (and lost on Stop). It now opens a fresh conversation in the same tab, and the running turn keeps going in the background. Typed `/cd <path>` now works in the web UI (it previously always failed with "not supported"). `/load <index>` mid-turn is refused with "press Stop first".
