# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- Fixed `tests::chat_welcome` insta snapshot: the banner-version redaction (`vX.Y.Z`) is one character shorter than `v0.10.0`, which shifted the rendered line's trailing-space padding by one cell and broke CI. Updated the snapshot and clarified the redaction's length-preservation requirement in a test comment.
- Added a `pdf_read` built-in tool: extract text or Markdown from a PDF file, with an optional 1-indexed inclusive `start_page`/`end_page` range (mirrors `file_read`'s line range) and a `format` of `text` (default) or `markdown`. Output truncates to 50,000 characters.
- Replaced the `pdfsink-rs` PDF dependency with `pdf_oxide` (pure-Rust, MIT/Apache) as the single PDF extraction library, used by both `pdf_read` and `doc_index`. Pinned to `default-features = false, features = ["legacy-crypto"]` so the dependency graph stays free of any C/`*-sys` crates and cross-compiles cleanly to all four targets (linux, windows, macOS, android).
