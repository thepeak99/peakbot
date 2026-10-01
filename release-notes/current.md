# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Fix: a session no longer wedges forever when a turn panics.** A detached PeakBot whose terminal had gone away (SSH closed) could panic on its first error log line: the log write failed, tracing-subscriber reported that via `eprintln!`, which panics on a dead stderr. The panic killed the agent loop, leaving the UI stuck "working" with Stop ignored. Logging now drops failed writes (`log_internal_errors(false)`), and the agent loop catches a panic while handling a message: the in-flight tool call is answered `INTERRUPTED`, the turn ends with `⚠ internal error: … — turn aborted`, and the session keeps serving.
