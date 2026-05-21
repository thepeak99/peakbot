# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- `bash_bg`: process **exit notifications now always reach the model**, even when the capped-tier circuit breaker is suppressing chatty output. Exits are one-shot terminal transitions that cannot loop, so the breaker has no protective value there; the suppression check now exempts exits explicitly. (regression pin in `bg_processes::tests::exit_notification_bypasses_capped_suppression`)
- `bash_bg`: rewrote the tool description and tightened the system-prompt line to teach the **event-driven contract** explicitly — the model is auto-woken on output and on exit, and must **not** `sleep N && bash_bg list` to wait. The correct pattern after `bash_bg start` is "end your turn; the next `[bg output]` turn is your cue." Also clarified that `capture_output_lines: 0` still delivers the exit notification.
- `agents.md` Tools Overview synced for `bash_bg` with the same no-poll + always-notify-on-exit guidance.
