# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Features

- **Per-profile tab colour.** Each profile now has a colour. Set it with `ui.color` (one of red, orange, yellow, green, teal, blue, purple, pink). If unset, a stable colour is derived from the profile name.
- Conversation tabs show a coloured top accent. The selection chip and conversation picker rows show a colour dot.
- Conversation list entries now carry their profile. The conversation index is rebuilt once on upgrade.
- **Remote profiles (config).** A profile can now point at another PeakBot server with `remote: { url, token, ca, profile, agent? }`. Rules:
  - `ca` is `sha256:<fingerprint>` or `system`.
  - Plain `http://` is allowed only for loopback.
  - Remote profiles are allowed only in the global config.
  - They are validated at load, and the token is never logged.
  - They are listed by `/profile` but can't be opened yet; opening from the web UI lands in a later release.
- **Server side of remote profiles.** The web socket's `attach` frame now accepts `profile` and `agent` and starts a new conversation on that pair (an unknown profile, an unknown agent or a `--profile` pin conflict is refused with an error). A conversation that already has turns keeps its saved selection. The `ready` frame carries a protocol version (`v`).
- New token-gated `GET /api/conversations` and `POST /api/conversations/{id}/kill` return this server's own conversation list.
- The HTTPS startup banner prints the CA fingerprint (`sha256:…`), the value a remote profile's `ca` pins.
