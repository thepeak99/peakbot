# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Android arm64 build target.** New `Dockerfile.android` cross-compiles a standalone `aarch64-linux-android` terminal binary (runnable in Termux or via `adb shell` — not an APK), using the Android NDK + `cargo-ndk`. Wired into the Makefile as `make build-android` / `release-build-android`, and included in the default `make build` and `make release` flows — the release now ships four artifacts (linux, windows, macos, android-arm64). Builds clean thanks to PeakBot's pure-rustls/ring TLS stack (no OpenSSL to cross-compile against Android's system SSL).

- **`thought` tool field is now a cross-cutting wrapper, not per-tool.** Every built-in and MCP tool is wrapped in a `ThoughtGate` that injects a required `thought` parameter into the tool schema, strips it before delegating, and appends a soft reminder to the result when it's omitted or blank — the tool still runs. Omitting `thought` no longer hard-fails the tool call. The `thought` field was removed from every individual tool's args/schema (the `think` tool keeps its own `thought`, which is its payload). MCP tool calls now surface a `thought` field and nudge too.
