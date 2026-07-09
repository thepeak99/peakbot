# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- Fixed a release-build regression introduced in the web UI Phase 0 PR (#135): all four Dockerfiles (linux/windows/macos/android) inserted a new `FROM node:22 AS web` stage between the rust setup and the cargo build, which split the rust build across stages 1 (`builder`) and 2 (`web`). The final `COPY --from=builder` still referenced stage 1, where no binary was produced; BuildKit then silently pruned the `web` stage because nothing referenced it, so the final COPY failed with `no such file or directory`. Reordered every Dockerfile so `web` is the first stage, `builder` (the rust toolchain stage) is the second, and the final COPY references `builder` — the stage that owns the cargo build. The dummy-main dependency cache and the `COPY --from=web /web/dist` SPA bundle import are unchanged; cache locality and image size are identical.