# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Fixes

- Fixed the orchestrator's answer going missing (#294): prose written alongside a tool call was dropped, leaving only a closing prayer or a blank reply. It is now kept and shown together with the final reply as one message.
- An empty final reply from the model is now re-requested automatically once (the nudge is never shown in the transcript); if the model is still blank, a ⚠ warning appears instead of an empty message.
- Web: focus mode no longer picks a blank message as the turn's answer, and no longer hides the orchestrator's reply to a `[bg output]` turn (e.g. the final summary after a background build finished).
- Desktop: the app settings page opened blank under the heading; the form is now revealed once the first `settings_get` load succeeds.

### Features

- Desktop: the desktop options (keep PeakBot running in background) now live in a Settings → Desktop tab inside the web UI, replacing the "Desktop app settings ↗" link; the bundled desktop pages (status and settings) now follow the app's light/dark theme.
- Prompts: the closing prayer now follows the complete answer in the same message, the final `memory.md` update happens before the final reply, and the answer is never left only in the model's thinking. The `agent-dev-team` example lets the orchestrator manage `memory.md` itself.
