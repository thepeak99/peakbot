# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Fixes

- Desktop (Linux): pasting an image (Ctrl+V) into the chat box now attaches it. The bundled WebKitGTK did not pass clipboard images to the page, so the app now reads the image from the clipboard itself. Text paste, the 📎 button, drag-and-drop and macOS/Windows/browser behaviour are unchanged.
