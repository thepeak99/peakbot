# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- File tools (`file_create`, `file_str_replace`, `file_insert`, `file_read`,
  `list_directory`, `pdf_read`) and `doc_index` now accept **relative paths**,
  not just absolute ones. Relative paths resolve against the session working
  directory (a shared `resolve_against` helper); the old "path is not absolute"
  rejection is gone. Absolute paths behave exactly as before.
- The `bash` and `powershell` tools now spawn their child process in a
  per-session working directory. Each call starts fresh from that directory —
  an in-command `cd` no longer leaks into later calls. The directory is a
  mandatory value set at construction (currently the process launch dir; a
  later change wires the real per-session value).
- The session working directory is now owned by a single source of truth (the
  state manager) rather than read from the process environment at each tool
  build. No behaviour change yet — this wiring lets a later change vary the
  directory per session without touching the process-global cwd.
- The system prompt now takes the working directory explicitly instead of
  reading the process environment. The `# Environment Information → Current
  Working Directory` line and the `agents.md` lookup are now governed by one
  passed directory (previously they agreed only because both read the process
  cwd). No behaviour change — every call site passes the same directory the
  process was in.