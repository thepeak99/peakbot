# PeakBot Mobile

**PeakBot Mobile** is a Tauri 2 shell (app id `sh.shifu.mobile`, display name PeakBot)
for Android and iOS. Its webview only ever loads the on-device PeakBot core
(`http://127.0.0.1:<port>`); remote servers are `remote:` profiles in the core's
`config.yaml`, served through the existing relay exactly like desktop, so there is
one conversation list. The native Profiles page is gone.

## Connect your server

- **The server** must run PeakBot 0.27.0 or newer with the same protocol version
  (version match is strict). On the server, open Settings → General → "Connect a
  phone or another PeakBot" to get a pairing link (it now includes the server's
  port, unless the server is on 443).
- **Add it:** in the app, go to Settings → Profiles → "+ Add remote profile", then
  paste the pairing link or tap **Scan QR** (phone only).
- **No CA install:** the pairing link's `ca: sha256:…` pins the server's CA, so
  there is nothing to install on the phone.

Going back on Android closes the Settings screen and the QR camera before leaving
the app.

## On this phone

**On this phone** (the local profile's default name) runs a PeakBot core built into
the app, in-process, with no code execution. It gets the read-only tools
`fetch_url`, `fetch_page`, `web_search` (only when SearXNG is configured), `todo`,
and `think`; takes image-only attachments; and uses **your own API key** — your
messages go to your configured provider with that key. A consent notice appears on
the provider step ("Agree and continue"). It never runs commands or edits files.
Your config and data stay on the phone and are excluded from backups.

On first launch the wizard offers either "Set up an AI provider on this device" or
"Connect to my PeakBot server" (no API key needed for the latter).

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

`mobile/` is the Tauri 2 app: `src-tauri/` (Rust glue + config), `ui/` (web UI
assets), `scripts/` (build, simulator, and CDP helpers), and `tests/` (contract
tests).
