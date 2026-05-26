# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **MCP OAuth 2.1 support — Slice 2 (#19).** PeakBot can now connect to
  streamable-HTTP MCP servers that require OAuth 2.1 + Dynamic Client
  Registration (RFC 7591) + PKCE (RFC 7636) — primary target:
  [Linear's MCP server](https://mcp.linear.app/mcp). Configure with
  `auth: { type: oauth }`; on first connect PeakBot opens your browser
  to authorise, then caches the tokens at
  `dirs::cache_dir()/peakbot/mcp-auth/<server-name>.json` (mode `0600`
  on Unix; group/world-readable caches are rejected on load with a
  remediation hint). Subsequent runs skip the browser entirely; rmcp's
  `AuthorizationManager` silently refreshes expired access tokens via
  the stored refresh token. The callback listener binds on
  `127.0.0.1:0` (ephemeral) and registers that exact port via DCR
  before opening the browser, so the redirect URI matches byte-for-byte.
  On SSH (`$SSH_CONNECTION` set) the URL is printed instead of opening
  a browser — forward the callback port with `ssh -L PORT:127.0.0.1:PORT`
  to complete the flow from a workstation. New module `src/mcp_auth.rs`
  with 9 unit tests (filesystem cred store round-trip, 0600 mode,
  world-readable reject, axum callback router happy + CSRF mismatch,
  `state` extraction, percent-decoder); test count up to **729**.

- **MCP auth config shape — Slice 1 of OAuth support (#19).** Added the
  locked `auth: { type: bearer | oauth, token?: "…" }` block on
  `mcp_servers[*]`. Bearer is a drop-in replacement for the legacy
  top-level `auth_token` field; `oauth` parses but bails at connect time
  with an actionable error until Slice 2 wires the flow through a new
  `src/mcp_auth.rs`. The legacy `auth_token` field keeps working for one
  release with a deprecation warning at connect time; setting both
  `auth_token` and `auth` is a hard config error. A defensive `Bearer `
  prefix strip lands in the same pass: pasting `auth_token: "Bearer xxx"`
  no longer produces a doubled `Authorization: Bearer Bearer xxx` header.
