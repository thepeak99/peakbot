# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Fix: a session no longer wedges forever when a turn panics.** A detached PeakBot whose terminal had gone away (SSH closed) could panic on its first error log line: the log write failed, tracing-subscriber reported that via `eprintln!`, which panics on a dead stderr. The panic killed the agent loop, leaving the UI stuck "working" with Stop ignored. Logging now drops failed writes (`log_internal_errors(false)`), and the agent loop catches a panic while handling a message: the in-flight tool call is answered `INTERRUPTED`, the turn ends with `⚠ internal error: … — turn aborted`, and the session keeps serving.

- **A panicking tool returns to its caller instead of aborting the turn.** Every tool call (built-in, MCP, sub-agent tools, and `delegate` itself) passes through `TimeBudget`, which now also catches panics and returns `💥 PANIC: tool \`X\` crashed: …` as a normal tool result, the same way it returns `⏱ TIMEOUT`. The orchestrator or sub-agent sees the error and decides what to do next. A panic inside a delegation surfaces to the orchestrator as the `delegate` result. The turn-level catch from #349 remains as a backstop for panics outside tools. In the TUI, the terminal is now restored only when the main thread panics, so a caught worker-thread panic no longer breaks the screen.
