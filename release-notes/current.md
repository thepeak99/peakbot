# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- Web UI: fixed the top and bottom bars being hidden behind mobile browser chrome (address bar / tab toolbar). The root container now uses `h-dvh` (dynamic viewport height) instead of `h-screen` (`100vh`), so the app fills the *currently visible* viewport instead of the chrome-collapsed maximum.
- Web UI: the on-screen keyboard now resizes the layout instead of pushing the top bar off-screen — added `interactive-widget=resizes-content` to the viewport meta, so `h-dvh` shrinks to the space above the keyboard and the app compresses in place.
