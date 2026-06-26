# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- Fixed the Android/Termux binary panicking at the first HTTPS request with
  "expect rustls platform verifier to be initialized". The build now targets
  `aarch64-unknown-linux-musl` (via cargo-zigbuild) instead of
  `aarch64-linux-android`, which selects rustls-platform-verifier's Unix
  (no-JVM) backend and produces a fully static binary. Runs zero-config in
  Termux. (`Dockerfile.android` no longer needs the Android NDK or cargo-ndk.)

- Fixed the Android/Termux binary panicking at startup with "No CA
  certificates were loaded from the system" on devices with no system trust
  store. The binary now embeds the Mozilla CA bundle and points
  `SSL_CERT_FILE` at it before any TLS client is built (`src/ca_certs.rs`), so
  every HTTPS client (provider, MCP, web fetch) finds roots with zero device
  setup. No-op when the host already has `SSL_CERT_FILE`/`SSL_CERT_DIR` set or
  a standard system bundle present.
