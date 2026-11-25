# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Cloud Mode

- **⚠ Update every linked device together.** The link between two of your devices moves to a new cloud pipe version, and an updated device and an older one cannot open sessions to each other: requests between a mixed pair fail until the second device is updated too. Nothing else needs to change — the relay needs no update and no data is migrated, so device identities, the device chain, enrollments and conversations are untouched; rolling back is safe too, as long as every device rolls back with it.
- **Two computers can now use each other in both directions, even when a stream is already open.** Once a cloud session exists, either side can open its own requests on it — ask a computer for its profiles or open a conversation while it already holds one to you. Before, only the device that started the session could ask for anything, so the other direction failed until the session was re-established. Each direction keeps its own stream, and a reset or failure on one side no longer disturbs the other side's open streams. A device that cannot serve requests at all (a phone with no local profiles) now refuses them at once, while its own requests keep working.

### Caching

- **Chats open faster on every device, and remote chats keep working while offline.** Each device now keeps a shared local cache of conversations: the 100 most recent across every linked machine, plus any chats you've opened. While online, the chat list stays current without polling, and opening a chat fetches only what's new — new messages arrive as a small suffix, with a moving anchor refreshed every 10 new rows so the cache never drifts. Cached chats stay readable offline, and when a computer or device is unlinked, its cached chats are automatically cleared.

### Fixes
- Mobile: pictures from agent `view_image` tool results now appear when tapping the tool result open, tap image for full-screen; desktop unchanged.
- Cloud: the phone now lists every linked computer separately — with its own device name and host color — even when several share the same profile name.
- Cloud: first-device, revoke, and sign-out confirm dialogs now close when the request completes; failures are shown in the pane.
- Cloud: first-device setup copy now explains the permanent device history and that every device has equal approval rights; the trust-mismatch banner no longer claims the root device changed.
- Cloud: requesting something from a device whose relay connection is gone now fails at once (its open streams are reset and pending requests answered) instead of waiting; a handshake that never completes is retired after 30 seconds instead of holding the session for a day, so a request to a device that has not been updated fails within 30 seconds instead of hanging.
