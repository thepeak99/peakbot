# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **`fetch_url` can POST (opt-in)** — new `fetch_url: { allow_post: true }` config (also per-repo and per-profile) adds optional `method`/`body`/`content_type` args; off by default, schema unchanged when off.
- **Attach any file in chat** — the web composer now takes any file type (📎, paste, or drag-and-drop anywhere on the chat panel) as a chip row with thumbnails, file-type icons, per-file upload progress and inline errors (limits: `uploads: { max_file_mb: 50, max_files: 10 }`, restart to apply). Attachments are stored per conversation under the PeakBot data dir, survive `/load`, and render in the transcript (image lightbox, downloadable file chips). The model gets images inline on vision models and a path list for everything else — non-vision models no longer reject images. `[img:/path]` and `[img:URL]` in the TUI now copy the file into the conversation; `[img:data:…]` tokens are no longer accepted.
- **Full `cargo test` no longer OOMs the machine** — test builds no longer emit debuginfo (`[profile.test] debug = false`, mirroring CI) and local builds are capped at 8 parallel jobs (`.cargo/config.toml`), so a cold full-suite build stays within RAM instead of thrashing the host.
- **Test runs cap at 8 harness threads; CI drops the now-redundant `CARGO_PROFILE_TEST_DEBUG` override** — `.cargo/config.toml` sets `RUST_TEST_THREADS = "8"` to bound per-binary test-harness thread parallelism (default is all cores), and CI drops the `CARGO_PROFILE_TEST_DEBUG` env (originally added for the rustc OOM in PR #353) because `[profile.test] debug = false` from PR #361 already covers it.
- Stopping a background process or a timed-out `bash` command now kills its whole process group (SIGHUP, then SIGKILL after 250 ms); children that ignore SIGHUP no longer linger.
- **Windows:** background processes (`bash_bg`) and the Git Bash `bash` tool no longer die instantly with 0xC0000142 — the ConPTY is now kept alive until the child exits — and they no longer hang with no output: `portable-pty` is pinned to 0.8.1 to avoid the ConPTY cursor-query (`ESC[6n`) deadlock that 0.9 introduced (wezterm/portable-pty#6783).
