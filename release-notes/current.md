# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Features

- **PeakBot Mobile: smoother screen changes.** Opening a chat, going back to Chats, and opening or closing Settings now slide and fade like a native messenger. Motion is off when the phone asks for reduced motion; desktop and browser are unchanged.
- **Chats list and the desktop conversations picker: consistent row states.** Running chats are now highlighted in amber with a spinner, live sessions keep their green highlight without the extra green dot, and the blue dot is the only marker (mobile: new messages).
- **Internal: new `peakbot-cloud` crate.** The pure (no I/O) core of the Cloud Mode protocol — device chain, six-word SAS, QR/link enrollment, enrollment letters, the SIGMA-style handshake and the data-frame codec — now lives in `crates/cloud`, frozen by 132 regression tests. The app does not use it yet (phase 1 of Cloud Mode).

### Fixes

- Mobile: each conversation keeps its own half-typed message again — switching chats from the chat list no longer carries text into the other conversation, and coming back restores your draft.
- Opening a different chat no longer flashes the previous chat's messages. Your five most recently viewed chats now open instantly and refresh in place; any other chat shows an empty screen for a moment while it loads. Applies to the mobile chat list, desktop tabs and the conversations picker.
