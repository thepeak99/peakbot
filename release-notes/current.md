# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Features

- **Android: releases now go to Google Play (internal track).** The release pipeline builds an upload-key-signed App Bundle and publishes it to the Google Play internal testing track (as a draft until the app is published). The bundle is Play-only; the 10 published release assets are unchanged.
- **PeakBot Mobile: message dates, and the working folder moves to the status line.**
  - On phones, the chat shows day dividers (Today, Yesterday, or the date) and a small time
    under each message. Tap the time to see the full date and time.
  - The line above the message box now always shows the working folder (📁 name). Tap it to
    open the new **Directory** panel (first in the panels sheet) and pick a folder.
  - The folder chip under the message box is gone. While a turn runs, the task and todo counts
    are shortened.
  - A profile can hide folder picking with `ui.tabs: { disabled: [directory] }`. This hides the
    folder chip on desktop too. If a profile uses `ui.tabs: { only: [...] }`, add `directory`
    to keep folder picking.
  - Remote servers need this version for their folder picker to show.
- **Cloud Mode (preview, API only): sign in, enroll devices, revoke, sign out.** With `general.cloud` set (restart required), `/api/cloud/*` lets this PeakBot log in to a relay, create the account's first device, show the six-word code a new device must match, approve or reject new devices (nothing is approved unless you explicitly choose it), revoke a device and sign out — a live feed of the state is at `/api/cloud/events`. No UI yet. Revoking or signing out removes a device's access at once.
- **Cloud Mode:** a computer can now be controlled from another of your devices through the relay (CloudServe, `profile@device` tabs, merged conversation list); CloudLink starts at boot when `general.cloud` is set. Fixed a cloud-protocol mux bug (End is a half-close).
- **Internal: remote-profile transport seam.** The remote-profile controller now rides an enum over transports (direct connection only, today) so Cloud Mode devices can join without touching the proxy, relay or listing code. User-visible: unknown `/r/` paths (anything that isn't an image or an upload fetch) are now refused with a local 404 without contacting the remote server.
- **PeakBot Mobile: smoother screen changes.** Opening a chat, going back to Chats, and opening or closing Settings now slide and fade like a native messenger. Motion is off when the phone asks for reduced motion; desktop and browser are unchanged.
- **PeakBot Desktop: notifications when a task finishes.** When a conversation finishes, fails or is stopped while the PeakBot window is in the background or hidden in the tray, you get a system notification with the conversation's name. Click it to jump straight back to that conversation. It works for every conversation on this PeakBot, including ones you started from your phone or a browser. To turn notifications off, set `general.notifications.enabled: false` (restart required) or use your system's notification settings.
- **Chats list and the desktop conversations picker: consistent row states.** Running chats are now highlighted in amber with a spinner, live sessions keep their green highlight without the extra green dot, and the blue dot is the only marker (mobile: new messages).
- **Internal: new `peakbot-cloud` crate.** The pure (no I/O) core of the Cloud Mode protocol — device chain, six-word SAS, QR/link enrollment, enrollment letters, the SIGMA-style handshake and the data-frame codec — now lives in `crates/cloud`, frozen by 132 regression tests. The app does not use it yet (phase 1 of Cloud Mode).
- **Internal: new `peakbot-relay` binary.** Cloud Mode relay: OIDC login, public-key device chain store, blind WSS routing between a user's devices, 15-min enrollment mailbox. Build-only, not shipped yet. CI now also tests the `cloud` and `relay` crates.
- **Browser notifications remember you.** The bell now stays on across reloads (as long as the browser still allows notifications), and pings also arrive when the PeakBot tab is visible but another window has focus. A new speaker button next to the bell plays a short sound when a task finishes (off by default). While a task runs, the tab title shows ⟳. The bell's tooltip now explains when HTTPS (and trusting the PeakBot CA on `--tls`) is needed, or how to unblock notifications in site settings. The bell is hidden inside the PeakBot desktop and mobile apps. PeakBot can now be installed from the browser as a standalone app.
- **Turn-finished events (groundwork for notifications).** PeakBot now announces every finished agent turn — in any conversation on the server, including background-output turns and turns that were stopped or failed — on a token-gated live feed, `GET /api/notify/stream` (Server-Sent Events). Each event names the conversation (id and title), how the turn ended and whether you or background output started it; message text is never included. Slash commands don't produce events. Desktop, browser and phone notifications will build on this. To turn it off: `general.notifications.enabled: false` (restart required).
- **PeakBot Mobile: panels bottom sheet.** On phones the right-edge tab rail is gone and the chat is full width. A new Panels button in the chat header — badged with running tasks plus open todos — opens a bottom sheet with the Session, Todo, Files, Tasks, Bash and Agents panels in a scrollable pill strip, draggable between a half-screen and a near-full height. While a turn runs, a floating "working… Stop" pill stays above the sheet. A phone browser gets the same sheet from a button in the top bar.
- **PeakBot Mobile: inline task status.** While a turn runs — or while tasks or todos are open — a small line above the composer shows "working… · N tasks · x/y todos". Tapping a count opens that panel. No full-width status bar.
- **PeakBot Mobile: selection and machine in the chat header.** The profile › agent › model line under the chat title is now the selection picker: tap it to open it as a bottom sheet (picking a model closes it; it locks after the first message). Chats on a remote server show the machine name in front of it. The bottom bar is gone in the phone app, except for the remote working-folder picker.
- **PeakBot Mobile: machine selector.** When you have remote servers, the Chats screen gains a machine picker — All machines, This device, and each remote server — with online dots and conversation counts. It filters the chat list and profile chips, tags rows with their machine, and new chats start on the selected machine (a machine that is offline shows "is offline" and can't be started). With All selected, the + button asks which machine to start on.
- **PeakBot Mobile: the phone now matches the approved design.** A fidelity pass aligned every phone screen to the approved mock: the chat header, composer and working line; the Chats list ("This device" for this machine, remotes by host only); message rendering — user bubbles, plain agent prose, collapsible tool rows, and code blocks with Copy and diff tints; the panels sheet chrome and the phone faces of the Todo, Tasks, Session, Files, Bash and Agents panels; the Profile/Agent/Model selection sheet; the restyled machine sheets; and a phone-only light palette. Desktop at 1024px and up is pixel-identical to before.
- **PeakBot Desktop: always the wide layout.** The desktop app keeps the wide layout at any window size, and its minimum window size is now 1024×600.

### Cloud Mode

- Cloud: a rejected enrollment now shows as rejected, with a Try again action that sends a fresh enrollment request.

### Fixes

- Phone app no longer shows a duplicate folder picker under the message box for remote profiles — the footer is removed at phone width; the working folder is reached via the activity-row 📁 → Directory tab.
- **Web tokens with special characters work.** Tokens containing characters like `+`, `&`, `%`, `;` or spaces were rejected by the desktop app and broke links and cookies; the server now accepts correctly encoded tokens, and the links it prints are encoded.
- Mobile: each conversation keeps its own half-typed message again — switching chats from the chat list no longer carries text into the other conversation, and coming back restores your draft.
- Opening a different chat no longer flashes the previous chat's messages. Your five most recently viewed chats now open instantly and refresh in place; any other chat shows an empty screen for a moment while it loads. Applies to the mobile chat list, desktop tabs and the conversations picker.
- Mobile: the composer now respects the phone's bottom safe area — on phones with a home-indicator bar, the input field no longer sits under the gesture area.
- **Large chats no longer kill the phone.** A 20,000-message conversation now sends ~66% fewer bytes to the browser and needs roughly 20 MB less memory, because tool call arguments the screen never shows are no longer sent. Building the task tree for a chat with many sub-agents went from ~0.6 s to ~9 ms per update, and transcript rows are measured without parsing every message's timestamp.
