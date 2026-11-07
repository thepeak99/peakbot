# PeakBot Mobile

**PeakBot Mobile** is a Tauri 2 shell (app id `sh.shifu.mobile`, display name PeakBot)
for Android and iOS. Its only native screen is the **Profiles** page; each profile
is just an origin the webview loads.

## Remote profiles

A Remote profile points at an existing PeakBot server; the phone loads the
server's own SPA over HTTPS.

- **Setup the server:** use a recent PeakBot — the release that ships PeakBot
  Mobile — with `general.web` set to `bind: 0.0.0.0:7823`, `tls: true`, and a web
  token.
- **Pair:** on the server, open Settings → General → "Connect a phone or another
  PeakBot" to get a pairing link. In the app, paste the link or scan its QR.
- **Trust the server:** the phone must trust the server's self-signed CA. The app
  has an *Install certificate…* shortcut that opens
  `https://host:7823/peakbot-ca.crt` in your browser, then install it:
  - **Android:** Settings → Security → Install a certificate → CA.
  - **iOS:** open `https://host:7823/peakbot-ca.crt` in Safari, then
    Settings → VPN & Device Management → install the profile →
    Settings → General → About → Certificate Trust Settings, and set it to full
    trust.
- **Off your LAN:** put the server on Tailscale and pair by its `*.ts.net` address
  with a real certificate — no CA install on the phone.

## On this phone

**On this phone** runs a PeakBot core built into the app, in-process, with no code
execution. It gets the read-only tools `fetch_url`, `fetch_page`, `web_search`
(only when SearXNG is configured), `todo`, and `think`; takes image-only
attachments; and uses **your own API key** — your messages go to your configured
provider with that key. A consent notice appears on first use. It never runs
commands or edits files. Your config and data stay on the phone and are excluded
from backups.

## Notify me when the agent finishes

The app sends no push notifications itself. To be told when a task is done, use a
**server-side channel** on your PeakBot server — a **Telegram bot** or **ntfy**. The
agent (or server) posts a short message to that channel at the end of a task, and
your phone receives it through the channel's own push. No app-side push code is
involved.

## Building

- **Android** — `make mobile-android` builds the APK in a container. Release
  signing is via the env vars `ANDROID_KEYSTORE_B64`, `ANDROID_KEYSTORE_PASSWORD`,
  `ANDROID_KEY_ALIAS`, and `ANDROID_KEY_PASSWORD`; without them the build is a
  debug APK.
- **iOS** — `make mobile-ios` runs on a Mac (Xcode 26.3) and reads
  `APPLE_DEVELOPMENT_TEAM`.
- **Simulator** — use `mobile/scripts/ios-sim.sh`.
- **Emulator** — drive the running app on the Android emulator with
  `mobile/scripts/cdp.py`.

## Not supported

Push notifications from the app, running code or editing files on the phone, and
pairing with plain-HTTP servers (HTTPS only).

## Repo layout

`mobile/` is the Tauri 2 app: `src-tauri/` (Rust glue + config), `ui/` (the
Profiles page), `scripts/` (build, simulator, and CDP helpers), and `tests/`
(contract tests).
