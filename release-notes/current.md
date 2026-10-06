# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **`fetch_url` can POST (opt-in)** — new `fetch_url: { allow_post: true }` config (also per-repo and per-profile) adds optional `method`/`body`/`content_type` args; off by default, schema unchanged when off.
- **Attach any file in chat** — the web composer now takes any file type (📎, paste, or drag-and-drop anywhere on the chat panel) as a chip row with thumbnails, file-type icons, per-file upload progress and inline errors (limits: `uploads: { max_file_mb: 50, max_files: 10 }`, restart to apply). Attachments are stored per conversation under the PeakBot data dir, survive `/load`, and render in the transcript (image lightbox, downloadable file chips). The model gets images inline on vision models and a path list for everything else — non-vision models no longer reject images. `[img:/path]` and `[img:URL]` in the TUI now copy the file into the conversation; `[img:data:…]` tokens are no longer accepted.
