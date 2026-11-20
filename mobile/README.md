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
and `think`, plus six file tools confined to the conversation's own folder
(reads the attached files, writes only into its `work/` subfolder, never
deletes); it accepts attachments of any file type, and uses **your own API key**
— your messages go to your configured provider with that key. A consent notice
appears on the provider step ("Agree and continue"). It never runs commands.
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

- **Android** — `make mobile-android` builds the APK in a container (`ANDROID_FORMAT=aab` builds the Play bundle instead). Release
  signing is via the env vars `ANDROID_KEYSTORE_B64`, `ANDROID_KEYSTORE_PASSWORD`,
  `ANDROID_KEY_ALIAS`, and `ANDROID_KEY_PASSWORD`; without them the build is a
  debug APK.
- **iOS** — `make mobile-ios` runs on a Mac (Xcode 26.3) and reads
  `APPLE_DEVELOPMENT_TEAM`.
- **Simulator** — use `mobile/scripts/ios-sim.sh`.
- **Emulator** — drive the running app on the Android emulator with
  `mobile/scripts/cdp.py`.

## Releasing to Google Play (internal)

`make release` also builds an upload-key-signed Android App Bundle (AAB) and
publishes it to the **internal testing** track of `sh.shifu.mobile`. The AAB is
Play-only: it is not one of the 10 release assets.

**One-time setup (a human, on a trusted workstation — never in the repo or CI):**

1. Generate the upload key and note its SHA-256 fingerprint:

   ```sh
   keytool -genkeypair -v -storetype PKCS12 -keystore peakbot-upload.jks \
     -alias upload -keyalg RSA -keysize 4096 -validity 10000 \
     -dname "CN=PeakBot Upload Key, O=Patchnotes"
   keytool -list -v -keystore peakbot-upload.jks -alias upload
   base64 -w0 peakbot-upload.jks > peakbot-upload.jks.b64
   ```

   Keep **two independent backups** of the `.jks` and its password (the team
   password manager, plus an offline encrypted copy on separate media), verify
   a restore with `keytool -list`, then delete the plaintext `.jks` and `.b64`.
   Losing the key is recoverable (Google holds the app-signing key) but an
   upload-key reset goes through Play support and takes days.
2. Create a Google Cloud service account (enable the *Google Play Android
   Developer API*) and a JSON key. In Play Console → Users & permissions invite
   its email for `sh.shifu.mobile` with only **View app information** and
   **Release to testing tracks**.
3. Set the Gitea secrets `ANDROID_KEYSTORE_B64` (the `.b64` file),
   `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS` (`upload`),
   `ANDROID_KEY_PASSWORD`, and `PLAY_SERVICE_ACCOUNT_JSON` (the raw JSON key),
   plus the Gitea **variable** `PLAY_RELEASE_STATUS` (`draft`).
4. **The first upload is manual.** Play registers the key that signs the first
   bundle as the upload key, so build it with the real secrets exported and
   upload it in the Play Console in the browser:

   ```sh
   make mobile-android ANDROID_FORMAT=aab   # → output/mobile/PeakBot-Mobile-<v>-android-arm64.aab
   ```

**versionCode** is a pure function of the tag (`scripts/play-version-code.sh`),
so release candidates sort below their stable release and codes only go up:
`(MAJOR*1000000 + MINOR*1000 + PATCH) * 100 + slot`, slot 99 for a stable
`X.Y.Z` and `N` for `X.Y.Z-rc.N` (0–98). Only `X.Y.Z[-rc.N]` is accepted.

| Tag | versionCode |
|---|---|
| `0.28.0` | 2800099 |
| `0.28.1-rc.2` | 2800102 |
| `0.28.1` | 2800199 |
| `1.0.0` | 100000099 |

**Status.** While the app is unpublished Play only accepts `draft` releases, so
`PLAY_RELEASE_STATUS` stays `draft`. After the app is published, flip the Gitea
variable to `completed`.

**Re-running.** The publish is idempotent (a versionCode already on Play is
skipped). To retry just the Play step on a machine that has the AAB:

```sh
make release-publish-play TAG=<v>      # needs PLAY_SERVICE_ACCOUNT_JSON in the env
```

## Not supported

Push notifications from the app, running code on the phone, and
pairing with plain-HTTP servers (HTTPS only).

## Repo layout

`mobile/` is the Tauri 2 app: `src-tauri/` (Rust glue + config), `ui/` (web UI
assets), `scripts/` (build, simulator, and CDP helpers), and `tests/` (contract
tests).
