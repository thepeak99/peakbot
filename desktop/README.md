# PeakBot Desktop

PeakBot Desktop is a small app window around the PeakBot web UI. It ships its own
PeakBot backend (the *sidecar*) and starts and stops it for you. Design and
contracts: [`DESIGN.md`](DESIGN.md).

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
`~/Library/Application Support/com.peakbot.peakbot/web-token`); the settings page
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
- First launch on macOS (unsigned build): `xattr -dr com.apple.quarantine "/Applications/PeakBot Desktop.app"`.

Before deleting the app, untick *Keep PeakBot running in background* if you don't
want PeakBot to keep running.
