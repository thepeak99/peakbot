# AgentRunner Cleanup - COMPLETED ✓

## Problem

The `AgentRunner` in `src/lib.rs` contained a `run()` method (~300 lines) that:
1. **Was dead code** - never called anywhere in the codebase
2. Had a misleading comment: "kept for backward compatibility"
3. Duplicated REPL functionality that already existed in `run_loop()` + `ReplUi`
4. Violated MVC architecture: AgentRunner was reading stdin/stdout directly

## What Was Done

### Phase 1: Removed Dead Code ✓
- [x] Deleted `run()` method (lines 663-1089, ~427 lines)
- [x] Deleted `print_recent_messages()` helper (lines 121-175, ~55 lines)
- [x] Deleted `truncate()` helper (lines 177-183, ~7 lines)

### Phase 2: Cleaned Up Imports ✓
- [x] Removed `std::io::{self, BufRead, Write}` (only used by `run()`)
- [x] Removed `tokio::time::{Duration, sleep}` (only used by `run()`)

### Phase 3: Updated Comments ✓
- [x] Cleaned up `handle_success()` comment (removed "For legacy mode (run)" reference)
- [x] Added `#[allow(unused)]` to `skills` field (kept for API compatibility)

### Phase 4: Verification ✓
- [x] `cargo check` passes with no errors
- [x] `cargo build --features tui` succeeds
- [x] All 49 tests pass

## Lines Removed

| Item | Lines | Count |
|------|-------|-------|
| `run()` method | 663-1089 | ~427 |
| `print_recent_messages()` | 121-175 | ~55 |
| `truncate()` | 177-183 | ~7 |
| Imports | 44, 48 | 2 lines |
| **Total** | | **~491 lines** |

## New Rule Added to Zen of Engineering

Added to `/home/exe/.agents/skills/zen-engineering-skill/SKILL.md`:

```
**Don't keep dead code for "backward compatibility" unless explicitly told.**
"Backwards compatibility" is the most commonly abused justification for dead code.
If code is actually serving backward compatibility, it's not dead — it's used.
If it's never called, it's rot, not compatibility. The phrase "kept for backward
compatibility" on unused code is a red flag: it implies value that doesn't exist,
preventing cleanup. Always verify: grep for callers. If there are none, it's not
compatibility — it's dead code with a misleading comment. You may flag this pattern,
but do not remove the code unless explicitly requested.
```

## Files Modified

| File | Changes |
|------|---------|
| `src/lib.rs` | Removed ~491 lines of dead code, cleaned imports, updated comments |
| `/home/exe/.agents/skills/zen-engineering-skill/SKILL.md` | Added new rule |

## Architecture After Cleanup

```
main.rs
├── Creates AgentRunner (Controller)
├── Creates UIs (Tui or ReplUi - Views)
├── Creates action_sender channel
└── Spawns: runner.run_loop(action_receiver) ← Controller

AgentRunner now correctly:
├── Receives UiAction enum from Views
├── Calls agent
└── Writes to StateManager (Model)
   └── Never touches stdin/stdout
```

## Pre-existing Warnings (Not Fixed)

The following warnings existed before this cleanup and were not addressed:
- `McpServerHandle.service` field never read
- `SessionHook.stats` field never read  
- `DelegateTool` fields/methods never used
- `ReplUi::send_action()` method never used
