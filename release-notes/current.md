# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Web favicon now reflects agent state.** While the agent is working the
  favicon is replaced with a small spinning yellow arc (~15 fps, ~16 px)
  rendered from a canvas and pushed back into `<link rel="icon">`; when the
  agent goes idle the original `/favicon.svg` is restored. The animation
  uses `setInterval` and a `useEffect` cleanup so it tears down on unmount
  and on every toggle — the previous shape leaked a re-scheduling rAF
  chain on every `isRunning → false` because the captured ID was consumed
  by the first frame.

- **Web sessions now stay alive while the agent is working, and expire only
  when fully idle (#158).** A `peakbot --web` session used to be reaped purely
  on its attached-socket count: closing the tab mid-turn (a long tool round, a
  slow `bash_bg`, a long stream) could kill the agent in flight, while a
  finished-but-tab-open session never expired. Reaping now keys off *quiescence*
  — no sockets attached **and** the agent not processing a turn **and** no live
  `bash_bg` children under the session. Any of those three keeps the session
  live and resets the idle clock; only when all three go quiet does the
  `web.session_ttl_secs` clock start (default lowered to **600 s / 10 min**).
  The reaper samples the live agent/bg signals on each tick — no turn-lifecycle
  events are pushed into the web-only registry, and no duplicate "agent running"
  state is introduced (it derives from `StateManager::is_running`). Reopening a
  tab mid-run reattaches to the still-running agent.

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
  re-adopts the same tree), the welcome banner, and every path-aware
  tool. Two web sessions in different directories now stay correct with
  no process-global cwd mutation.
- The vestigial `SessionDeps.system_prompt` field is removed. The boot-cwd
  prompt used to live on `SessionDeps` (built once in `main.rs` and reused
  by every web session); the per-session flow made it dead and now it is
  gone. The per-session system prompt is built inside `create_session`
  from the resolved `session_cwd` — the only place the cwd is allowed to
  flow into the prompt. No behaviour change.
- `/cd` and `/load` no longer mutate the process-global cwd. Both commands
  now flip `state_manager.session_cwd` (per-session) and rebuild the agent
  on the new tree. The source-tree grep test `no_set_current_dir_in_src`
  is the lock, and `test_cd_handlers_never_mutate_process_cwd` proves
  the SM-level contract. Two web sessions in different trees now stay
  race-free with no shared global touched; the process cwd is the
  inherited launch dir and is irrelevant to correctness. `/new` and
  `/model` (and bare `/cd`) also read the session cwd from the SM
  instead of `current_dir()`, so every conversation-mint site is now a
  single source of truth.
- The system prompt no longer tells the model to "use absolute paths for
  all file operations". The new rule names the path-aware tools
  (`file_read`, `file_create`, `file_str_replace`, `file_insert`,
  `list_directory`, `pdf_read`, `doc_index`, the spawned shells) and
  says the model can pick: absolute or relative to the session working
  directory. The runtime has accepted relative paths since the file-tool
  rework and has run shells in the session cwd since the per-session
  wiring landed; this is the moment the prompt and the runtime agree.