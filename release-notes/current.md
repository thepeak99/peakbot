# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **`fetch_url` can POST (opt-in)** — new `fetch_url: { allow_post: true }` config (also per-repo and per-profile) adds optional `method`/`body`/`content_type` args; off by default, schema unchanged when off.
