# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- Web UI: new **Focus mode** toggle (top bar, on by default). Hides tool calls, tool results, background turns and other intermediate steps, showing only your messages, the final answer of each turn, and any errors. While the agent works, a single live activity line shows the latest step. Preference is remembered per browser.
- Windows: `peakbot install` no longer fails with "Access is denied (os error 5)". The install copy now opens the destination with a single write handle (copy + chmod + `sync_all` on it); Windows `FlushFileBuffers` requires write access, so the old read-only reopen failed.
- Windows: `peakbot service install` now works over SSH. The Task Scheduler user is resolved with `whoami` (falling back to `USERNAME` + `COMPUTERNAME`), because under Windows OpenSSH the `USERDOMAIN` environment variable is `WORKGROUP`, which `schtasks` cannot map to an account.
