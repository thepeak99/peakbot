# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

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
