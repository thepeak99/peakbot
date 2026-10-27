# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Fixes

- Web: focus mode no longer hides the orchestrator's reply to a `[bg output]` turn (e.g. the final summary after a background build finished).
- Desktop: the app settings page opened blank under the heading; the form is now revealed once the first `settings_get` load succeeds.

### Features

- Desktop: the desktop options (keep PeakBot running in background) now live in a Settings → Desktop tab inside the web UI, replacing the "Desktop app settings ↗" link; the bundled desktop pages (status and settings) now follow the app's light/dark theme.
