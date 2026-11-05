# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Fixes

- **Per-profile tab colour.** Each profile now has a colour. Set it with `ui.color` (one of red, orange, yellow, green, teal, blue, purple, pink). If unset, a stable colour is derived from the profile name.
- Conversation tabs show a coloured top accent. The selection chip and conversation picker rows show a colour dot.
- Conversation list entries now carry their profile. The conversation index is rebuilt once on upgrade.
- **Remote profiles (config).** A profile can now point at another PeakBot server with `remote: { url, token, ca, profile, agent? }`. Rules:
  - `ca` is `sha256:<fingerprint>` or `system`.
  - Plain `http://` is allowed only for loopback.
  - Remote profiles are allowed only in the global config.
  - They are validated at load, and the token is never logged.
  - They are listed by `/profile` in the TUI as web-only. See "Remote profiles in the web UI" below.
- **Server side of remote profiles.** The web socket's `attach` frame now accepts `profile` and `agent` and starts a new conversation on that pair (an unknown profile, an unknown agent or a `--profile` pin conflict is refused with an error). A conversation that already has turns keeps its saved selection. The `ready` frame carries a protocol version (`v`).
- New token-gated `GET /api/conversations` and `POST /api/conversations/{id}/kill` return this server's own conversation list.
- **Remote profiles (relay + proxy).** A socket attached to a remote profile is relayed to that server. The conversation list now merges remote rows (tagged with the local profile name, newest first; a down remote simply contributes no rows), and killing a remote conversation is routed to its server. Remote uploads and images stream through the token-gated `/r/{profile}/…` routes. Remote profiles named `.` or `..` are a config error.
- The HTTPS startup banner prints the CA fingerprint (`sha256:…`), the value a remote profile's `ca` pins.
- **Remote profiles in the web UI.** Remote profiles are now selectable in the web UI.
  - They show next to your local profiles in the profile picker, marked `remote` and colour-coded.
  - Remote profiles are now selectable in the profile picker (the remote picks its agent).
  - Clicking a remote profile in the picker connects it and lists the remote's real agents.
  - Each tab can be local or remote, side by side. Picking a profile on the other side of that line starts a new conversation on it.
  - The conversation list merges local and remote conversations into one list. Opening or killing a remote row goes to its own server.
  - Uploads and images are proxied through your local server.
  - The remote's token and pinned CA stay on the local server. The browser never sees them.
  - The protocol versions must match exactly; a mismatch is refused with an error naming both versions.
  - The TUI and stdio refuse remote profiles.
- **Remote profiles in Settings.** The Profiles pane lists remote entries with a `remote` badge and their tab colour, and edits them (URL, masked token, CA pin, target profile, optional agent pin, tab colour) — with a `ca` required hint when the pin is empty. "+ Add remote profile" pastes a pairing link and creates the entry named `<profile> on <host>`. Settings → General gains a "Connect a phone or another PeakBot" panel: the pairing links (host labels only, the token URL behind Show QR), a profile dropdown, and Copy link.
- Desktop (Linux): pasting an image (Ctrl+V) into the chat box now attaches it. The bundled WebKitGTK did not pass clipboard images to the page, so the app now reads the image from the clipboard itself. Text paste, the 📎 button, drag-and-drop and macOS/Windows/browser behaviour are unchanged.
- `/stats` no longer double-counts API calls and per-agent token totals for the main agent.
