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
| `make desktop-windows` | `PeakBot-Desktop-<v>-windows-x86_64-setup.exe` |
| `make desktop` | all three |

`make desktop-windows` cross-compiles with `cargo-xwin` (MSVC target, no Windows
machine or wine) and bundles an NSIS installer with the bundled PeakBot sidecar.
The first run downloads the MSVC SDK (cached in the build layers) and takes a while.

**Windows build gotchas** (both already solved in `Dockerfile.desktop-windows`,
details in [`DESIGN.md` §5.7](DESIGN.md#57-build-contracts)): the base image must be
Ubuntu 24.04, because Tauri's NSIS installer script needs `Win\RestartManager.nsh`,
which only exists in NSIS ≥ 3.09 (22.04 ships 3.08 and the bundler aborts with a
cryptic `!include` error); and the build image must install
`libayatana-appindicator3-dev`, because the Tauri CLI (a Linux host binary) probes
for an appindicator library via pkg-config and panics without it, even when the
target is Windows.

## Windows

- **Unsigned build → SmartScreen warning.** The installer is not code-signed, so
  Windows shows "Windows protected your PC". Click **More info → Run anyway**.
- **WebView2.** The app window uses Microsoft Edge WebView2, which is preinstalled
  on Windows 10 (1803+) and Windows 11. On a machine without it the installer
  downloads the WebView2 bootstrapper (Tauri's default `downloadBootstrapper` mode),
  so that machine needs internet access during install.
- **Background mode** installs a Task Scheduler logon task named `PeakBot`
  (not a Windows service). A console window opens at sign-in and stays open — that
  is PeakBot running; minimise it rather than closing it.

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
any web page, #378). The token is written to `web-token` (mode `0600`) and never to
`settings.json`, the service unit or the command line. Saving a new token restarts
PeakBot (or reinstalls the background service) after a confirm dialog, like any
other setting.

### HTTPS

PeakBot signs its own certificate with its own CA
(`~/.cache/peakbot/tls/ca-cert.pem`). Other browsers and phones work after you
accept it once.

- **Linux:** the app window trusts the certificate **automatically** — only when it
  is presented by PeakBot's own backend on this computer and is cryptographically
  signed by that CA. Anything else still gets the normal certificate error page. If
  the CA file is missing, the app window shows the certificate error; restore the
  file (or switch to HTTP in Settings) and press Retry.
- **macOS:** not yet. The app window **shows a certificate error** until the
  certificate is trusted (#376, deferred). Open Settings (tray, menu, or
  Cmd+Shift+S) and switch back to HTTP, or reset the settings (below).

If you set `web.tls: true` in PeakBot's `config.yaml`, PeakBot serves HTTPS no
matter what the Settings page says, and the app (which then probes plain HTTP)
reports that PeakBot did not start. Remove `web.tls` from `config.yaml` and let
the Settings page control the protocol.

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

## Manual removal

- Linux: `peakbot service uninstall` (or `systemctl --user disable --now peakbot.service; rm ~/.config/systemd/user/peakbot.service; systemctl --user daemon-reload`);
  optionally `rm ~/.local/bin/peakbot` and `rm -r ~/.local/share/com.peakbot.desktop`.
  The settings in `~/.config/peakbot` are user data — delete them only deliberately.
- macOS: `~/.local/bin/peakbot service uninstall` (or `launchctl bootout gui/$(id -u)/com.peakbot.agent; rm ~/Library/LaunchAgents/com.peakbot.agent.plist`);
  optionally `rm ~/.local/bin/peakbot` and
  `rm -r ~/Library/Application\ Support/com.peakbot.desktop ~/Library/Logs/com.peakbot.desktop`.
- Windows: untick *Keep PeakBot running in background* first (or run
  `peakbot service uninstall`, or `schtasks /Delete /TN PeakBot /F`) — the uninstaller
  does not remove the logon task. Then uninstall *PeakBot Desktop* from Settings → Apps;
  optionally delete `%APPDATA%\com.peakbot.desktop`. Settings in
  `%APPDATA%\peakbot` are user data — delete them only deliberately.
- First launch on macOS (unsigned build): `xattr -dr com.apple.quarantine "/Applications/PeakBot Desktop.app"`.

Before deleting the app, untick *Keep PeakBot running in background* if you don't
want PeakBot to keep running.
