# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **PeakBot Mobile for Android and iOS** (built in `mobile/`). A Tauri 2 shell
  (app id `sh.shifu.mobile`, name PeakBot) whose webview now loads only the on-device
  PeakBot core; remote servers are `remote:` profiles relayed exactly like desktop,
  so there is one conversation list. The native Profiles page is gone.
  - **On this phone** — a built-in PeakBot core with no code execution: fetch/search,
    todo, think, file attachments, your own API key, consent on the provider
    step; config and data stay on the phone and are excluded from backups.
  - **Connect a server** — Settings → Profiles → "+ Add remote profile", then paste
    the pairing link or scan its QR (phone only). No CA install: the pairing
    link's `ca: sha256:…` pins the server CA, and the link now carries the server
    port unless 443. Servers must run PeakBot 0.27.0 or newer with the matching
    protocol version.
  - The first-launch wizard offers "Set up an AI provider on this device" or
    "Connect to my PeakBot server" (no API key needed for the latter).
  - **Android 17:** PeakBot Mobile now asks for the "Nearby devices" (local network) permission so it can reach LAN servers; if denied, the timeout error says where to turn it on.
  Core changes this release: `fetch_url` gains a `headers` argument; provider
  `base_url` values with a trailing slash are now tolerated; and the web SPA gains
  a `window.peakbotShell` marker (desktop / mobile / plain browser).

- **PeakBot Mobile: attach any file.** The phone app now accepts any file type, not just images. On Android, 📎 opens a short menu — take a photo, choose from the gallery, or pick a file (also in the web UI on Android browsers); on iPhone and iPad, 📎 opens the system's own sheet. On this phone, the agent can read the files you attach and keep notes in the conversation's own folder (nothing else on the device). Remote profiles accept files too; size and count limits are the server's. Old servers that only take images now say so clearly.

### Fixes

- Web UI: images now display on remote profiles, including `view_image` tool results, which previously showed only a `🖼 alt` text stub. Uploaded-file thumbnails that fall back to the served file now use the remote profile's route too.

- Desktop: "Check for updates…" (macOS app menu and tray) now only checks — it no longer downloads and installs an already-known update immediately. When an update is found you get a dialog, and installing happens from the update banner.

- **Mandatory Discord release announcement.** Every release (stable and RC) now gets exactly one Discord embed-card post as the final step, only after the release is verified (all 10 Gitea assets, shifu `SHA256SUMS`, GitHub `latest` tag for stables). One format: an embed card that always pings @everyone via `content` + `allowed_mentions` (a mention inside an embed does not ping), posted to the `DISCORD_WEBHOOK_AI` webhook with `?wait=true`. Full format, template, and limits are defined in `agents.md` › "Release announcement (mandatory)".

- Web UI: changing directory (picker or typed `/cd <path>`), typing `/new`, or `/load <id>` while a turn is running no longer gets silently queued until the turn ends (and lost on Stop). It now opens a fresh conversation in the same tab, and the running turn keeps going in the background. Typed `/cd <path>` now works in the web UI (it previously always failed with "not supported"). `/load <index>` mid-turn is refused with "press Stop first".

- On the phone, the folder picker is now available when working on a remote profile.

- Web UI: a dead chat connection is now noticed and fixed on its own — the server sends a quiet liveness beat every 20 s, and a page that hears nothing for over 45 s drops the socket and dials again. It reconnects at once when the tab or app returns to the foreground or the network comes back, and on the phone the page is also told to reconnect when the app resumes.
