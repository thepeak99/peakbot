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
- `Conversation::new` and `StateManager::create_conversation` now take `cwd`
  explicitly as the last argument. The implicit `std::env::current_dir()` read
  inside `Conversation::new` is gone; every caller (`ensure_boot_conversation`,
  `/new`, `/model`, `/cd`) passes its own value, behaviour-preserving today.
  This makes the directory a first-class constructor field and lets later
  changes wire the real per-session value without touching every mint site
  again. The empty string is still a valid value (and what the in-memory
  `ConversationManager::create_new` continues to forward, matching its
  historical behaviour).
- `create_session` now resolves and freezes the per-session `session_cwd`
  exactly once at construction: resume adopts the saved cwd (must exist and
  be a directory), fresh mints inherit the boot `current_dir()`. That single
  value flows into the system prompt (built from it, not from
  `SessionDeps.system_prompt`), the persisted conversation (so `/load`
  re-adopts the same tree), the welcome banner, and — via Phase 3 — every
  path-aware tool. Two web sessions in different directories now stay
  correct with no process-global cwd mutation. The frozen
  `SessionDeps.system_prompt` field is now unused in the per-session path
  and is slated for removal in the next phase.