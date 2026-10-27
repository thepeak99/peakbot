# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Fixes

- Fixed the orchestrator's answer going missing (#294): prose written alongside a tool call was dropped, leaving only a closing prayer or a blank reply. It is now kept and shown together with the final reply as one message.
- An empty final reply from the model is now re-requested automatically once (the nudge is never shown in the transcript); if the model is still blank, a ⚠ warning appears instead of an empty message.
- Web: focus mode no longer picks a blank message as the turn's answer, and no longer hides the orchestrator's reply to a `[bg output]` turn (e.g. the final summary after a background build finished).
- PeakBot Desktop (macOS, Windows): with HTTPS on (`general.web.tls: true`) the app window no longer hangs on 'Opening PeakBot…'. On the first start the app adds PeakBot's own certificate authority to your user's trusted certificates (the system asks you to approve); if you decline, the window explains why and offers Retry. Server: the HTTPS leaf certificate is now valid for one year (re-minted every start), as macOS requires.

### Features

- PeakBot Desktop: closing the window now keeps PeakBot running in the tray (menu bar on macOS); choose **Quit PeakBot** to stop it. The "Keep PeakBot running in background" option and the desktop settings page are gone; if an old background service is installed, the app offers to remove it on start (settings and the `peakbot` command are kept). Linux desktops without a tray ask before closing, since closing stops PeakBot.
- Desktop: the bundled status page follows the app's light/dark theme.
- Prompts: the closing prayer now follows the complete answer in the same message, the final `memory.md` update happens before the final reply, and the answer is never left only in the model's thinking. The `agent-dev-team` example lets the orchestrator manage `memory.md` itself.
