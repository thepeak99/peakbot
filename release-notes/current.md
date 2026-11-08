# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **PeakBot Mobile for Android and iOS** (built in `mobile/`). A Tauri 2 shell
  (app id `sh.shifu.mobile`, name PeakBot) whose webview now loads only the on-device
  PeakBot core; remote servers are `remote:` profiles relayed exactly like desktop,
  so there is one conversation list. The native Profiles page is gone.
  - **On this phone** — a built-in PeakBot core with no code execution: fetch/search,
    todo, think, image-only attachments, your own API key, consent on the provider
    step; config and data stay on the phone and are excluded from backups.
  - **Connect a server** — Settings → Profiles → "+ Add remote profile", then paste
    the pairing link or scan its QR (phone only). No CA install: the pairing
    link's `ca: sha256:…` pins the server CA, and the link now carries the server
    port unless 443. Servers must run PeakBot 0.27.0 or newer with the matching
    protocol version.
  - The first-launch wizard offers "Set up an AI provider on this device" or
    "Connect to my PeakBot server" (no API key needed for the latter).
  Core changes this release: `fetch_url` gains a `headers` argument; provider
  `base_url` values with a trailing slash are now tolerated; and the web SPA gains
  a `window.peakbotShell` marker (desktop / mobile / plain browser).

### Fixes

- Desktop: "Check for updates…" (macOS app menu and tray) now only checks — it no longer downloads and installs an already-known update immediately. When an update is found you get a dialog, and installing happens from the update banner.

- **Mandatory Discord release announcement.** Every release (stable and RC) now gets exactly one Discord embed-card post as the final step, only after the release is verified (all 10 Gitea assets, shifu `SHA256SUMS`, GitHub `latest` tag for stables). One format: an embed card that always pings @everyone via `content` + `allowed_mentions` (a mention inside an embed does not ping), posted to the `DISCORD_WEBHOOK_AI` webhook with `?wait=true`. Full format, template, and limits are defined in `agents.md` › "Release announcement (mandatory)".
