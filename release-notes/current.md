# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- Markdown links in agent replies now show their destination URL: `[text](url)` renders the anchor text (underlined) followed by the bare `url` in a dim style, so terminals that auto-detect URLs make it click-to-open. Autolinks and non-web schemes (relative paths, fragments) are left untouched to avoid noise.
