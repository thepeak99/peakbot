# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Features

- **PeakBot Mobile: smoother screen changes.** Opening a chat, going back to Chats, and opening or closing Settings now slide and fade like a native messenger. Motion is off when the phone asks for reduced motion; desktop and browser are unchanged.
- **PeakBot Desktop: notifications when a task finishes.** When a conversation finishes, fails or is stopped while the PeakBot window is in the background or hidden in the tray, you get a system notification with the conversation's name. Click it to jump straight back to that conversation. It works for every conversation on this PeakBot, including ones you started from your phone or a browser. To turn notifications off, set `general.notifications.enabled: false` (restart required) or use your system's notification settings.
- **Chats list and the desktop conversations picker: consistent row states.** Running chats are now highlighted in amber with a spinner, live sessions keep their green highlight without the extra green dot, and the blue dot is the only marker (mobile: new messages).
- **Turn-finished events (groundwork for notifications).** PeakBot now announces every finished agent turn — in any conversation on the server, including background-output turns and turns that were stopped or failed — on a token-gated live feed, `GET /api/notify/stream` (Server-Sent Events). Each event names the conversation (id and title), how the turn ended and whether you or background output started it; message text is never included. Slash commands don't produce events. Desktop, browser and phone notifications will build on this. To turn it off: `general.notifications.enabled: false` (restart required).

### Fixes

- Mobile: each conversation keeps its own half-typed message again — switching chats from the chat list no longer carries text into the other conversation, and coming back restores your draft.
- Opening a different chat no longer flashes the previous chat's messages. Your five most recently viewed chats now open instantly and refresh in place; any other chat shows an empty screen for a moment while it loads. Applies to the mobile chat list, desktop tabs and the conversations picker.
