# PeakBot Desktop

PeakBot Desktop is a small app window around the PeakBot web UI. It ships its own
PeakBot backend (the *sidecar*) and starts and stops it for you. Design and
contracts: [`DESIGN.md`](DESIGN.md).

## Building

Desktop artifacts are cross-built on Linux in containers (podman by default;
`CONTAINER_BUILDER=docker` to override). Output lands in `output/desktop/`.

| Command | Artifact |
|---|---|
| `make desktop-linux` | `PeakBot-Desktop-<v>-linux-x86_64.AppImage` |
| `make desktop-macos` | `PeakBot-Desktop-<v>-macos-universal.zip` |
| `make desktop-windows` | `PeakBot-Desktop-<v>-windows-x86_64.exe` |
| `make desktop` | all three |

`make desktop-windows` cross-compiles with `cargo-xwin` (MSVC target, static CRT,
no Windows machine or wine). The PeakBot sidecar is embedded in the exe as a
compressed payload, so the result is a single portable file. The first run
downloads the MSVC SDK (cached in the build layers) and takes a while.

## Windows

- **One file, no install.** `PeakBot-Desktop-<v>-windows-x86_64.exe` runs from
  anywhere — double-click it (from Downloads, a USB stick, a read-only folder).
  No installer, no admin rights.
- **Unsigned build → SmartScreen warning.** The exe is not code-signed, so
  Windows shows "Windows protected your PC". Click **More info → Run anyway**.
- **WebView2 required.** The app window uses Microsoft Edge WebView2, which is
  preinstalled on Windows 11 and on almost every Windows 10. On the rare machine
  without it, the app shows a dialog with the download link.
- **First launch unpacks the backend.** The app extracts the bundled PeakBot
  backend to `%LOCALAPPDATA%\com.peakbot.desktop\bin\` (about 40 MB, once per
  version; older versions are removed automatically).
- **Background mode is not available on Windows yet.** The "Keep PeakBot
  running in background" option is therefore not shown on Windows; the app runs
  in the foreground (window + tray) and the backend exits with it.

## Settings

Open **Settings** with any of:

- the tray menu item `Settings…`
- the `PeakBot` menu → `Settings…` (macOS)
- **Ctrl+Shift+S** (**Cmd+Shift+S** on macOS)
- the `Settings` link on the status page

| Setting | What it does |
|---|---|
| Protocol | HTTP or HTTPS (PeakBot's own certificate; `--tls`). |
| Access token | Require the access token to open PeakBot. When on, the page shows the current token; type your own, press **Generate** for a random one, or **Copy** it. Leave it empty to keep the current token (or generate one). |
| Remote access | Listen on all network interfaces (`0.0.0.0`) so other devices can connect. Forces HTTPS and the access token. |
| Keep PeakBot running in background | Installs/removes the background service. Applies immediately. |

Saving protocol, token or remote asks for confirmation and then **restarts PeakBot**
(or reinstalls the background service); conversations that are running at that
moment are interrupted.

### Token

The access token is mandatory **only for remote access**. On this computer you can
turn it off, but then any program or web page on it can control PeakBot (see
#378) — leave it on unless you have a reason. The token lives in PeakBot's own
`web-token` file (Linux `~/.config/peakbot/web-token`, macOS
`~/Library/Application Support/com.peakbot.peakbot/web-token`, Windows
`%APPDATA%\peakbot\peakbot\config\web-token`); the settings page
shows the exact path and the current value. Turning the token off removes that file,
and the terminal `peakbot` command then runs without a token too. It is not rotated
on every launch.

**Setting your own token.** Type it into the field and press **Save**. A token may
contain only letters, digits, `-`, `_` and `.`, and must be **16 to 256 characters**
(this applies to loopback as well — a short token on this computer is guessable by
any web page, #378).

### HTTPS

When HTTPS is on — `general.web.tls: true` in PeakBot's `config.yaml`
(PeakBot → Settings → General) — the backend serves with a certificate signed by
its own CA ("PeakBot Local CA", `~/.cache/peakbot/tls/ca-cert.pem`).

- **Linux:** the app window trusts the CA **automatically** — no system prompt.
- **macOS / Windows:** on the first HTTPS start, the app adds "PeakBot Local CA"
  to this user account's trusted certificates. The system asks you to approve
  (Touch ID or password on macOS; a "Security Warning" on Windows) while the
  window shows "Approve PeakBot's certificate in the system prompt…". Declining
  leaves the window on a page that explains why and offers **Retry** — the
  alternative is `general.web.tls: false` in `config.yaml`.

To remove the CA again: macOS → Keychain Access → **login** → delete
"PeakBot Local CA"; Windows → `certmgr.msc` → **Current User** →
**Trusted Root Certification Authorities** → delete "PeakBot Local CA".

If `XDG_CACHE_HOME` differs between the app and the background service's
environment, they use different CAs and the Linux app window shows the certificate
error.

### Where settings live, and how to reset

Settings are saved in `settings.json` in the app's data folder:

- Linux: `~/.local/share/com.peakbot.desktop/settings.json`
- macOS: `~/Library/Application Support/com.peakbot.desktop/settings.json`
- Windows: `%APPDATA%\com.peakbot.desktop\settings.json`

**To reset to the defaults (HTTP, token on, remote off), quit the app and delete
`settings.json`.** A missing or unreadable file always loads as the defaults.
Whether the background service is on is never stored there; it is read from the
service manager.

## Background service updates

When the app starts and a background service is installed, it checks whether the installed binary is **exactly** the build the app bundles, by comparing file contents (not just the version number). If they differ, the app replaces `~/.local/bin/peakbot` and the service automatically — no dialog — keeping the service's HTTP/HTTPS mode and access token. Two exceptions: if the installed PeakBot is *newer* than the app's, the app asks first (and remembers a "Keep" answer), and development builds (built with `make desktop-dev`) always ask, so a debug binary can never silently overwrite your installed `peakbot`. If the binary can't be written, the existing service is left running and the app shows the error.

## Bundled emoji font (Linux AppImage)

The Linux AppImage bundles Google's **Noto Color Emoji** font
(`NotoColorEmoji.ttf`, CBDT bitmap format) together with its **SIL Open Font
License 1.1** text (`OFL.txt`) under `usr/share/peakbot-desktop/fonts/` in the
AppImage. It is used only on systems whose emoji font is in a format the
bundled web engine can't render (COLRv1 on recent Fedora): at startup the app
points `FONTCONFIG_FILE` at a small generated config that adds the bundled
font and hides outline color fonts. If you set your own `FONTCONFIG_FILE`,
the app leaves it alone. The font is © the Noto Color Emoji contributors and
remains licensed under OFL-1.1; the full license text ships with the app.

## Manual removal

- Linux: `peakbot service uninstall` (or `systemctl --user disable --now peakbot.service; rm ~/.config/systemd/user/peakbot.service; systemctl --user daemon-reload`);
  optionally `rm ~/.local/bin/peakbot` and `rm -r ~/.local/share/com.peakbot.desktop`.
  The settings in `~/.config/peakbot` are user data — delete them only deliberately.
- macOS: `~/.local/bin/peakbot service uninstall` (or `launchctl bootout gui/$(id -u)/com.peakbot.agent; rm ~/Library/LaunchAgents/com.peakbot.agent.plist`);
  optionally `rm ~/.local/bin/peakbot` and
  `rm -r ~/Library/Application\ Support/com.peakbot.desktop ~/Library/Logs/com.peakbot.desktop`.
- Windows: quit the app, then delete the `PeakBot-Desktop-<v>-windows-x86_64.exe`
  file; optionally delete `%LOCALAPPDATA%\com.peakbot.desktop` (the unpacked
  backend + settings) and `%APPDATA%\com.peakbot.desktop` (settings). There is
  no installer and no background task to remove. Settings in `%APPDATA%\peakbot`
  are user data — delete them only deliberately.
- First launch on macOS (unsigned build): `xattr -dr com.apple.quarantine "/Applications/PeakBot Desktop.app"`.

Before deleting the app, untick *Keep PeakBot running in background* if you don't
want PeakBot to keep running.
