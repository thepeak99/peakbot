# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Fixed `peakbot --web` hanging with a dead port when the browser can't be opened.** On a local session the server tries to auto-open the browser; it used `open::that`, which *waits* for the spawned handler (`xdg-open`) to exit. On systems where `xdg-open` blocks (misconfigured/absent MIME handler), that stalled the async runtime **before** `axum::serve` was ever reached — the port was bound so connections were accepted into the backlog, but every request hung with 0 bytes received. Switched to the fire-and-forget `open::that_detached`, so serving starts regardless of what the browser handler does.
