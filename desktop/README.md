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
| Access token | Require the access token to open PeakBot. |
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
shows the exact path. Turning the token off removes that file, and the
terminal `peakbot` command then runs without a token too. It is not rotated on
every launch.

### HTTPS

PeakBot signs its own certificate. Browsers and phones work after you accept it
once, but **the app window may show a certificate error** until the app trusts
that certificate (#376). If that happens, open Settings (tray, menu, or
Ctrl+Shift+S) and switch back to HTTP, or reset the settings (below).

If you set `web.tls: true` in PeakBot's `config.yaml`, PeakBot serves HTTPS no
matter what the Settings page says, and the app (which then probes plain HTTP)
reports that PeakBot did not start. Remove `web.tls` from `config.yaml` and let
the Settings page control the protocol.

### Where settings live, and how to reset

Settings are saved in `settings.json` in the app's data folder:

- Linux: `~/.local/share/com.peakbot.desktop/settings.json`
- macOS: `~/Library/Application Support/com.peakbot.desktop/settings.json`

**To reset to the defaults (HTTP, token on, remote off), quit the app and delete
`settings.json`.** A missing or unreadable file always loads as the defaults.
Whether the background service is on is never stored there; it is read from the
service manager.

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
