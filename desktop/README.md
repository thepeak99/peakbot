# PeakBot Desktop

PeakBot Desktop is a small app window around the PeakBot web UI. It ships its own
PeakBot backend (the *sidecar*) and runs it for you while the app is open. Closing the
window keeps it in the tray; **Quit PeakBot** stops it. Design and
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
- **Closing the window keeps PeakBot running.** The app runs in the tray (see
  [Quit and the tray](#quit-and-the-tray)); the window is hidden, not closed. Choose
  **Quit PeakBot** in the tray menu to stop it.

## Quit and the tray

PeakBot runs in the background of the desktop app. **Closing the window hides it**;
PeakBot (the server, and any jobs it is running) keeps running.

- **Show it again:** click the tray icon, or choose **Open PeakBot**. Starting the app
  a second time also brings the window back.
- **Stop PeakBot:** choose **Quit PeakBot** in the tray menu (macOS: **PeakBot → Quit
  PeakBot** or **Cmd+Q**). This stops the server and any running jobs.
- **Windows:** left-click the tray icon to show the window; right-click for the menu.
- **macOS:** the icon is in the menu bar. Closing the window with the red button hides it;
  click the Dock icon to bring it back.
- **Linux without a tray:** some desktops (e.g. GNOME without an AppIndicator extension)
  have no tray. There, closing the window asks **Close PeakBot?** first, because closing
  stops PeakBot. **Cancel** keeps it running.

## Upgrading from the background service

Earlier versions could keep PeakBot running as a system service (systemd / launchd,
"Keep PeakBot running in background"). That option has been removed. PeakBot Desktop now
runs PeakBot itself, so an old background service would conflict with it.

When the app starts and finds the old service installed (running or stopped), it asks
**Remove the old background service?**. Choose **Remove service** and the app removes it.
If you choose **Not now**, the app shows *The old PeakBot background service is still
installed* and does not start PeakBot until you press **Retry** (which asks again) or run:

```
peakbot service uninstall
```

Your settings, your conversations and the `peakbot` command are kept.

## HTTPS

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

Before deleting the app, quit it from the tray (**Quit PeakBot**). If you still have an
old background service installed, remove it first with `peakbot service uninstall`.
