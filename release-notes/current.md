# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **UI**: the `🛰 N bg` satellite indicator is now shown in the **idle** title bar (single-line and multiline input modes), not only while the agent is working. Previously `bg_segment` was computed inside the `is_running = true` arm only, so background processes were invisible during idle turns. (regression pin in `repl_impl.rs` snapshot tests)
- `bash_bg`: process **exit notifications now always reach the model**, even when the capped-tier circuit breaker is suppressing chatty output. Exits are one-shot terminal transitions that cannot loop, so the breaker has no protective value there; the suppression check now exempts exits explicitly. (regression pin in `bg_processes::tests::exit_notification_bypasses_capped_suppression`)
- `bash_bg`: rewrote the tool description and tightened the system-prompt line to teach the **event-driven contract** explicitly — the model is auto-woken on output and on exit, and must **not** `sleep N && bash_bg list` to wait. The correct pattern after `bash_bg start` is "end your turn; the next `[bg output]` turn is your cue." Also clarified that `capture_output_lines: 0` still delivers the exit notification.
- `agents.md` Tools Overview synced for `bash_bg` with the same no-poll + always-notify-on-exit guidance.
- **Shell detection**: PeakBot now auto-detects the best available shell at runtime and exposes only ONE shell tool to the model. Detection priority: `PEAKBOT_SHELL` env override → WSL → Git Bash → PowerShell 7+ (`pwsh`) → PowerShell 5.1 (`powershell`) → Unix `/bin/sh` fallback. On Windows with no shell found, a startup warning is shown but PeakBot continues (file editing and other tools still work). The detected shell is stored in `StateManager` and used by both the synchronous shell tool and `bash_bg` background processes. The `/model` rebuild path preserves the shell choice across switches.
- **Added `README.md`** — polished project readme with feature overview, quick-start guide, configuration examples, usage samples, architecture diagram, and development instructions. Co-authored by PeakBot!
