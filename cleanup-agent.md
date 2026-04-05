# Cleanup Agent Output: Analysis and Plan

## Executive Summary

**Goal**: Remove all `println!`/`eprintln!` from AgentRunner and related backend components, routing all output through `StateManager` where UI implementations can pull from.

**Current Problem**: The agent layer mixes concerns - it prints directly to stdout, which violates MVC architecture and causes duplicate output with the streaming handler.

---

## Current State Analysis

### Files with Print Statements

| File | Count | Type | Concern |
|------|-------|------|---------|
| `src/lib.rs` (AgentRunner) | ~40 | println!/eprintln! | **Controller printing** |
| `src/hooks/streaming_output_handler.rs` | ~15 | println! | **View concern in hooks** |
| `src/context_manager.rs` | 1 | eprintln! | Warning during compaction |

### Print Statement Locations in AgentRunner

#### 1. Public "print" methods (Controller → UI concern)
```rust
// These should return data, not print
print_stats()         // lines 217-226
print_context_status() // lines 229-237
print_todo_summary()   // lines 240-253
list_conversations()  // lines 256-282
```

#### 2. Internal print statements (Controller → mixed concerns)
```rust
// Status messages (should go to StateManager)
"[Stop requested...]"           // line 466
"[Agent stopped by user]"       // line 556
"[Context approaching limit...]" // line 587
"[Compacted: ...]"              // line 591

// Errors (should go to StateManager)
"Max number of retries exceeded" // line 642
"Error, retrying..."            // line 645
"Warning: compaction failed"     // line 597

// Command responses (should go to StateManager or return data)
"Stats reset."                  // line 670
"Context compaction not enabled" // lines 673, 676, 679, 694, etc.
"Started a new conversation."   // line 692
"Conversation saved:"           // line 701
"--- Loaded conversation: ---"  // line 731
"Conversation deleted."         // line 759
"Export ---"                    // line 803
"Unknown command:"              // line 837
```

### StreamingOutputHandler Problem

This handler in `src/hooks/streaming_output_handler.rs`:
- **Location**: Hooks module (should be backend)
- **Purpose**: Prints thinking, talking, tool calls to console
- **Problem**: This is a **VIEW concern** living in the backend

It causes **duplicate output**:
1. AgentRunner prints via println!
2. StreamingOutputHandler prints via println!
3. TUI/REPL may also render from StateManager

---

## MVC Architecture Analysis

```
┌─────────────────────────────────────────────────────────────────────┐
│                           CURRENT STATE                             │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│   View (TUI/REPL) ──────► Controller (AgentRunner) ─────► Model      │
│         │                        │                      (StateManager)│
│         │                        │                              │    │
│         │◄──────────────────────┼──────────────────────────────┘    │
│         │   StateManager         │                                    │
│         │                        │                                    │
│         │              ┌─────────┴─────────┐                         │
│         │              │                   │                         │
│         │              ▼                   ▼                         │
│         │    ┌──────────────────┐  ┌──────────────────┐             │
│         │    │ println!/eprintln!│  │StreamingHandler  │             │
│         │    │   (DUPLICATE!)   │  │   (DUPLICATE!)   │             │
│         │    └──────────────────┘  └──────────────────┘             │
│         │                                                           │
└─────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────┐
│                          DESIRED STATE                               │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│   View (TUI/REPL) ──────► Controller (AgentRunner) ─────► Model      │
│         │                        │                      (StateManager)│
│         │                        │                              │    │
│         │◄───────────────────────┼──────────────────────────────┘    │
│         │   StateManager         │                                    │
│         │   (sole source)         │                                    │
│         │                        │                                    │
│         │              ┌─────────┴─────────┐                         │
│         │              │                   │                         │
│         │              │   No direct print │                         │
│         │              │   No streaming    │                         │
│         │              │   handler         │                         │
│         │              └───────────────────┘                         │
│         │                                                           │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Zen of Engineering Applied

### 1. **Simplicity** - Remove duplication
> "Duplication is far cheaper than the wrong abstraction"

**Problem**: Two places print to console (AgentRunner + StreamingOutputHandler)
**Solution**: Remove StreamingOutputHandler, have UI render from StateManager only

### 2. **Single Responsibility** - Controller doesn't output
> "A class should have only one reason to change"

**AgentRunner should**:
- Receive UiActions
- Call the agent
- Write results to StateManager
- Never print

**AgentRunner should NOT**:
- Print anything directly
- Know about console output

### 3. **Least Astonishment** - Predictable data flow
> "Your API is a user interface for developers"

When a user sends a message:
1. StateManager receives it
2. AgentRunner processes it
3. StateManager broadcasts updated state
4. UI renders from StateManager

### 4. **Don't Be Clever** - Explicit over implicit
> "If you need a comment, the code is too clever"

Current: "We print from agent AND from streaming handler, trust us it's fine"
Desired: "All output comes from StateManager, observable and debuggable"

---

## Implementation Plan

### Phase 1: Extend StateManager/AppState

Add fields to `AppState` for transient messages:

```rust
// In src/ui/app_state.rs

/// Notification for user-facing messages (confirmations, status updates)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: Uuid,
    pub message: String,
    pub kind: NotificationKind,
    pub timestamp: DateTime<Local>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NotificationKind {
    Info,
    Success,
    Warning,
    Error,
}

/// Extend AppState
pub struct AppState {
    // ... existing fields ...
    
    /// Pending notifications for the UI to display
    pub notifications: Vec<Notification>,
    
    /// Agent status message (e.g., "Compacting...", "Stopped")
    pub status_message: Option<String>,
}
```

Add methods to `StateManager`:

```rust
impl StateManager {
    /// Push a notification to be displayed by UI
    pub fn push_notification(&self, message: String, kind: NotificationKind) {
        let notification = Notification {
            id: Uuid::new_v4(),
            message,
            kind,
            timestamp: Local::now(),
        };
        let mut state = self.state.write().unwrap();
        state.notifications.push(notification);
        self.notify_update(&state);
    }
    
    /// Clear all notifications
    pub fn clear_notifications(&self) { ... }
    
    /// Set agent status message
    pub fn set_status(&self, message: Option<String>) { ... }
    
    /// Add to chat (already exists, verify it's used)
    pub fn add_system_message(&self, content: String) {
        let msg = ChatMessage::system(content);
        self.update_chat(msg);
    }
}
```

### Phase 2: Update AgentRunner

Replace all `println!`/`eprintln!` with StateManager calls:

```rust
// BEFORE
println!("[Stop requested...]");

// AFTER
if let Some(ref sm) = state_manager {
    sm.push_notification("Stop requested...".to_string(), NotificationKind::Info);
}
```

Or for errors:
```rust
sm.push_notification(format!("Error: {}", e), NotificationKind::Error);
```

For command responses that should display in chat:
```rust
sm.add_system_message("Conversation saved.".to_string());
```

### Phase 3: Remove StreamingOutputHandler

This handler prints to console from the hooks module. Options:

**Option A**: Remove entirely
- Simplest
- UIs render from StateManager
- No more duplicate output

**Option B**: Convert to no-op / make configurable
- Keep for debugging
- Default to disabled
- Environment variable `PEAKBOT_DEBUG_STREAM=1`

**Recommendation**: Option A - remove completely. Debug logging should use `tracing!` (already in use), not println.

### Phase 4: Remove print_* public methods

Simply delete these methods - UI pulls data from StateManager directly:

```rust
// DELETE these methods:
pub fn print_stats(&self) { ... }
pub fn print_context_status(&self) { ... }
pub fn print_todo_summary(&self) { ... }
pub fn list_conversations(&self) { ... }
```

The UI accesses this data via:
- `StateManager.get_state().stats` - session statistics
- `StateManager.get_state().context` - context status
- `StateManager.get_state().todo` - todo list
- `StateManager.get_state().conversation` - conversation info

### Phase 5: Context Manager

The single `eprintln!` in `context_manager.rs` should use `tracing::warn!` instead:

```rust
// BEFORE
eprintln!("Warning: Failed to summarize messages: {}. Falling back to truncation.", e);

// AFTER
tracing::warn!("Failed to summarize messages: {}. Falling back to truncation.", e);
```

---

## File Changes Summary

| File | Change |
|------|--------|
| `src/ui/app_state.rs` | Add `Notification`, `NotificationKind` types; extend `AppState` |
| `src/ui/state_manager.rs` | Add `push_notification()`, `set_status()`, `add_system_message()` methods |
| `src/lib.rs` | Replace all `println!`/`eprintln!` with StateManager calls; remove `print_*` methods |
| `src/hooks/streaming_output_handler.rs` | **Delete entire file** |
| `src/hooks/mod.rs` | Remove `StreamingOutputHandler` export |
| `src/context_manager.rs` | Replace `eprintln!` with `tracing::warn!` |

---

## Testing Considerations

1. **Unit tests** can still use `println!` - these are tests, not production code
2. **Integration tests** should verify StateManager receives correct notifications
3. **Manual testing** should verify TUI/REPL display notifications from StateManager

---

## Migration Checklist

- [ ] Add `Notification` and `NotificationKind` to `app_state.rs`
- [ ] Add `notifications` and `status_message` to `AppState`
- [ ] Add `push_notification()`, `set_status()`, `add_system_message()` to `StateManager`
- [ ] Replace all `println!` in `AgentRunner` with StateManager calls
- [ ] Remove `print_stats()`, `print_context_status()`, `print_todo_summary()`, `list_conversations()`
- [ ] Replace `eprintln!` in `context_manager.rs` with `tracing::warn!`
- [ ] Delete `streaming_output_handler.rs`
- [ ] Remove `StreamingOutputHandler` export from `hooks/mod.rs`
- [ ] Remove `StreamingOutputHandler` from `main.rs` setup
- [ ] Update REPL UI to render notifications
- [ ] Update TUI to render notifications
- [ ] Verify build passes
- [ ] Manual testing of TUI and REPL modes

---

## After This Change

The architecture will be clean:

```
User Types Message
       │
       ▼
┌──────────────────┐
│   TUI/REPL View  │ ◄── Subscribes to StateManager
└────────┬─────────┘
         │ sends UiAction
         ▼
┌──────────────────┐
│  AgentRunner     │ ◄── Controller (NO PRINT)
│  (Controller)    │
└────────┬─────────┘
         │ writes to
         ▼
┌──────────────────┐
│  StateManager    │ ◄── Single source of truth
│  (Model)         │
└────────┬─────────┘
         │ broadcasts
         ▼
┌──────────────────┐
│   TUI/REPL View  │ ◄── Renders state
└──────────────────┘
```

All output is observable, debuggable, and consistent across UI implementations.
