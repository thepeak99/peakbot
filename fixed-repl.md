# Fixed-Prompt REPL with Interruptible Agent — Refactor Plan

**Date**: 2026-04-03  
**Goal**: Enable user interaction during agent processing (interrupt, interleave messages) by improving REPL UX and adding cancellation support.

---

## TL;DR

This refactor adds three capabilities:
1. **Fixed prompt** — Input always stays at the bottom of the terminal, output scrolls above
2. **Message interleave** — Queue additional messages while agent is processing; they execute sequentially after current task completes
3. **Agent cancellation** — Stop the running agent via Ctrl+C, see pending queue, resume or continue

**Key architectural decision**: REPL is reimplemented using **ratatui** (same library as TUI) instead of ANSI escape sequences. This ensures cross-platform compatibility and code reuse.

**Testing-first approach**: Tests are implemented immediately after each feature phase to ensure correctness.

---

## Current Architecture

```
┌─────────────────────────────────────────────────────────┐
│ stdin (thread) ────────► UiAction ────────► Controller │
│                               │                         │
│                               ▼                         │
│                      ┌────────────────┐                 │
│                      │ StateManager  │                 │
│                      │ (Model)       │                 │
│                      └────────────────┘                 │
│                               │                         │
│                               ▼                         │
│ stdout ◄─────── broadcast ◄─── View (REPL)            │
└─────────────────────────────────────────────────────────┘
```

**Current REPL limitations**:
- Spawns a thread for stdin, subscribes to StateManager
- Simple `print!("> ")` — no fixed-position prompt
- No terminal control, no scrolling management
- No way to send additional messages during processing
- No cancellation mechanism for the agent

**Current TUI** (for reference):
- Uses ratatui with crossterm backend (cross-platform)
- Full layout with title bar, chat area, TODO panel, input area, status bar
- Already has InputHandler for key events

---

## Architectural Decision: Unified UI with ratatui

### Why ratatui for REPL?

| Approach | Pros | Cons |
|----------|------|------|
| **ANSI escape sequences** | No new deps, lightweight | Windows cmd.exe compatibility issues, manual cursor tracking |
| **ratatui (chosen)** | Cross-platform (Windows/macOS/Linux), shared code with TUI, mature library | Slightly heavier, full layout model |

### New Architecture

```
┌─────────────────────────────────────────────────────────┐
│                      UiAction Channel                    │
│  SendMessage │ CancelCurrent │ InterruptAndInject │ Exit │
└─────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────┐
│                    AgentRunner (Controller)             │
│  - is_running: Arc<AtomicBool>                        │
│  - pending_queue: Vec<String>                          │
│  - current_task: Option<JoinHandle>                   │
│                                                              │
│  State Machine:                                          │
│  ┌──────────┐    send_msg    ┌──────────────┐          │
│  │  IDLE    │ ────────────► │  PROCESSING  │          │
│  └──────────┘               └──────────────┘          │
│       ▲                           │                     │
│       │ ctrl+c                    │ done               │
│       └───────────────────────────┘                     │
└─────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────┐
│                   StateManager (Model)                  │
│  - AppState with chat, input, stats, context, todo    │
│  - Broadcasts state changes to subscribers             │
└─────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────┐
│                      Views (ratatui)                    │
│  ┌─────────────────────┐  ┌─────────────────────┐       │
│  │      TUI View      │  │     REPL View      │       │
│  │  Full layout       │  │  Simplified layout │       │
│  │  - Title bar       │  │  - Chat scroll     │       │
│  │  - Chat area       │  │  - Fixed input     │       │
│  │  - TODO panel      │  │  - Queue status    │       │
│  │  - Input area      │  │  - Interrupt UI    │       │
│  │  - Status bar      │  └─────────────────────┘       │
│  └─────────────────────┘                               │
└─────────────────────────────────────────────────────────┘
```

### REPL Layout (ratatui)

```
┌──────────────────────────────────────────────────────────────────────────┐
│ PeakBot REPL                                                    [Ctrl+C]│
├──────────────────────────────────────────────────────────────────────────┤
│ │ Chat messages scroll here...                                        │
│ │ Previous output is preserved above...                                │
│ │ ...                                                                 │
│ │                                                                      │
│ │                                                                      │
│ │                                                                      │
├──────────────────────────────────────────────────────────────────────────┤
│ > [user input here]                                          [Send]   │
├──────────────────────────────────────────────────────────────────────────┤
│ Processing: "explain the code"... │ Queue: 2 │ Ctrl+C interrupt        │
└──────────────────────────────────────────────────────────────────────────┘
```

### Interrupt Overlay

When Ctrl+C is pressed during processing:

```
┌──────────────────────────────────────────────────────────────────────────┐
│ INTERRUPTED                                                             │
├──────────────────────────────────────────────────────────────────────────┤
│ │ [Agent output so far...]                                              │
│ │ [Tool calls...]                                                       │
│ │                                                                        │
│ │ ──────────────────────────────────────────────────────────────────── │
│ │ Pending Messages (2):                                                 │
│ │   1. continue                                                          │
│ │   2. also show me tests                                                │
│ │                                                                        │
│ │ Options:                                                               │
│ │   [Enter] Continue with next message                                   │
│ │   [r]     Resume agent (same context)                                  │
│ │   [c]     Clear queue                                                  │
│ │   [x]     Exit                                                         │
├──────────────────────────────────────────────────────────────────────────┤
│ > [user input here]                                                     │
└──────────────────────────────────────────────────────────────────────────┘
```

---

## Implementation Phases

### Phase 1: ratatui REPL Foundation

**Goal**: Reimplement REPL using ratatui (same library as TUI). This gives us cross-platform support and shared code patterns.

#### Technical Approach

1. **New REPL module structure**:
   ```
   src/ui/repl/
   ├── mod.rs           # Exports
   ├── ratatui_impl.rs  # Main REPL using ratatui
   └── renderer.rs      # REPL-specific rendering
   ```

2. **REPL Layout**:
   ```
   Layout (vertical):
   ├── Constraint::Length(1)        # Title bar "PeakBot REPL [Ctrl+C]"
   ├── Constraint::Fill(1)         # Chat/scroll area
   ├── Constraint::Length(input)   # Input area (dynamic height)
   └── Constraint::Length(1)       # Status bar
   ```

3. **Key components to reuse from TUI**:
   - `tui/input_handler.rs` — handles crossterm events, Ctrl+C mapping
   - `tui/renderer.rs` — can be adapted, or create shared components
   - `StateManager` — already broadcasts state to all subscribers

4. **New AppState fields for REPL**:
   ```rust
   pub struct AppState {
       // ... existing fields ...
       
       // REPL-specific
       pub repl: ReplState,
   }
   
   pub struct ReplState {
       pub is_processing: bool,
       pub current_message: Option<String>,
       pub pending_queue: Vec<String>,
       pub is_interrupted: bool,
       pub status_message: String,
   }
   ```

#### Changes

| File | Changes |
|------|---------|
| `src/ui/repl/ratatui_impl.rs` | New REPL implementation using ratatui |
| `src/ui/repl/renderer.rs` | REPL-specific rendering components |
| `src/ui/app_state.rs` | Add `ReplState` struct |
| `src/ui/repl/mod.rs` | Export new ratatui-based REPL |

#### Acceptance Criteria
- [ ] REPL compiles and runs with ratatui
- [ ] Fixed layout: title → chat → input → status
- [ ] Chat area scrolls when content overflows
- [ ] Input area stays fixed at bottom
- [ ] Cross-platform: works on Windows, macOS, Linux

---

### Phase 2: Snapshot Tests for REPL Foundation

**Goal**: Add automated snapshot tests immediately after Phase 1 to verify the REPL renders correctly.

#### Reference Implementation
See `~/workz/tuitest` for a complete working example of ratatui snapshot testing with `insta`.

#### Technical Approach

1. **Use `ratatui::backend::TestBackend`** for in-memory rendering:
   ```rust
   use ratatui::{backend::TestBackend, Frame, Terminal};
   
   fn render_to_string(app: &mut ReplState) -> String {
       let backend = TestBackend::new(80, 24);
       let mut terminal = Terminal::new(backend).expect("Failed");
       terminal.draw(|f| repl_ui(f, app)).expect("Failed");
       
       // Extract buffer content
       let buffer = terminal.backend().buffer();
       // Convert to string lines...
   }
   ```

2. **Use `insta` for snapshot assertions**:
   ```rust
   use insta::assert_snapshot;
   
   #[test]
   fn test_repl_empty_state() {
       let mut state = ReplState::new();
       let output = render_to_string(&mut state);
       assert_snapshot!(output);
   }
   ```

#### Test Categories

1. **Rendering Tests** (snapshots):
   - Empty state
   - Single user message
   - Single agent message
   - Multiple messages (conversation)
   - Long messages (wrapping)
   - Input buffer with various content

2. **Status Bar Tests** (snapshots):
   - Idle state (no processing)
   - Processing state (with message preview)

3. **Input Tests** (snapshots):
   - Empty input (placeholder)
   - Single line input
   - Multi-line input
   - Long input (wrapping)

#### Changes

| File | Changes |
|------|---------|
| `tests/repl_snapshots.rs` | Snapshot tests for REPL rendering |
| `tests/snapshots/*.snap` | Snapshot files (auto-generated) |
| `Cargo.toml` | Add `insta` dev-dependency |

#### Snapshot Files Location
```
tests/
├── snapshots/
│   ├── repl_test__empty_state.snap
│   ├── repl_test__with_user_message.snap
│   ├── repl_test__processing_state.snap
│   ├── repl_test__input_empty.snap
│   ├── repl_test__input_with_content.snap
│   └── ...
```

#### Running Tests
```bash
# Run all snapshot tests
cargo test --test repl_snapshots

# Update snapshots (when intentionally changing output)
INSTA_UPDATE=always cargo test
```

#### Acceptance Criteria
- [ ] Snapshot tests compile and pass
- [ ] Each REPL state variant has a snapshot
- [ ] New snapshots can be created with `INSTA_UPDATE=always`

---

### Phase 3: Async Task Management

**Goal**: Decouple input reading from agent execution so we can handle Ctrl+C and queue messages.

#### Technical Approach

1. **Add UiAction variants**:
   ```rust
   pub enum UiAction {
       SendMessage(String),
       ExecuteCommand(String),
       CancelCurrent,                  // NEW: abort running task
       InterruptAndInject(String),     // NEW: cancel + add to queue
       InterruptResume,                // NEW: resume after interrupt
       InterruptContinue,              // NEW: continue with next queued message
       InterruptClearQueue,            // NEW: clear pending queue
       Exit,
   }
   ```

2. **Add task management to AgentRunner**:
   ```rust
   pub struct AgentRunner {
       // ... existing fields ...
       
       // NEW: Task management
       pub current_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
       pub pending_queue: Arc<Mutex<Vec<String>>>,
       pub is_running: Arc<AtomicBool>,
       pub is_interrupted: Arc<AtomicBool>,
   }
   ```

3. **Message processing loop**:
   ```rust
   impl AgentRunner {
       pub async fn run_loop(&mut self, mut action_receiver: mpsc::UnboundedReceiver<UiAction>) {
           loop {
               // Check pending queue first (if not interrupted)
               if !self.is_interrupted.load(Ordering::SeqCst) {
                   if let Some(msg) = self.pop_pending() {
                       self.process_message(msg).await;
                       continue;
                   }
               }
               
               // Wait for next action
               match action_receiver.recv().await {
                   Some(UiAction::SendMessage(msg)) => {
                       if self.is_running() {
                           self.queue_message(msg);
                       } else {
                           self.process_message(msg).await;
                       }
                   }
                   Some(UiAction::CancelCurrent) => self.cancel_current(),
                   Some(UiAction::InterruptAndInject(msg)) => {
                       self.cancel_current();
                       self.queue_message(msg);
                   }
                   Some(UiAction::InterruptResume) => {
                       self.is_interrupted.store(false, Ordering::SeqCst);
                   }
                   Some(UiAction::InterruptContinue) => {
                       self.is_interrupted.store(false, Ordering::SeqCst);
                       self.process_next_pending().await;
                   }
                   Some(UiAction::InterruptClearQueue) => {
                       self.clear_pending_queue();
                   }
                   None | Some(UiAction::Exit) => break,
                   _ => {}
               }
           }
       }
   }
   ```

#### Changes

| File | Changes |
|------|---------|
| `src/ui/ui_trait.rs` | Add new UiAction variants |
| `src/lib.rs` | Add task management to AgentRunner |

#### Task Cancellation Strategy

Since Rig's agent doesn't support cancellation natively, we wrap agent execution in a spawned task:

```rust
async fn process_message(&mut self, msg: String) {
    let agent = self.agent.clone();
    let history = self.chat_history.clone();
    
    self.is_running.store(true, Ordering::SeqCst);
    
    let handle = tokio::spawn(async move {
        agent.prompt_with_history(&msg, &mut history).await
    });
    
    self.current_task = Some(handle);
    
    // Wait for completion
    let result = self.current_task.take().unwrap().await;
    
    self.is_running.store(false, Ordering::SeqCst);
}

On cancellation:
1. Call `handle.abort()`
2. Don't clear `chat_history` — preserve context
3. Set `is_interrupted = true`
4. Update StateManager so UI shows interrupt overlay
```

#### Acceptance Criteria
- [ ] Multiple messages can be queued
- [ ] CancelCurrent stops current task
- [ ] Queue processes sequentially
- [ ] Interrupt preserves context

---

### Phase 4: SIGINT + Interrupt UI

**Goal**: Allow Ctrl+C to interrupt the agent and show an interactive overlay for queue management.

#### Technical Approach

1. **SIGINT handling in REPL**:
   - Use `tokio::signal::ctrl_c()` in the main loop via `tokio::select!`
   - When Ctrl+C received, send `UiAction::CancelCurrent` to the channel
   - Set `is_interrupted = true` in StateManager

2. **Interrupt overlay in REPL renderer**:
   - When `is_interrupted = true`, show overlay instead of normal input
   - Display pending queue, options
   - Handle keypresses for options (Enter, r, c, x)

3. **Interrupt state machine**:
   ```
   ┌─────────────────┐
   │    IDLE         │ ← Normal state
   └────────┬────────┘
            │
   Ctrl+C or SendMessage while processing
            │
            ▼
   ┌─────────────────┐
   │  INTERRUPTED    │ ← Shows overlay
   └────────┬────────┘
            │
     User chooses:
     - [Enter] → Continue with next in queue
     - [r]     → Resume (restart with same context)
     - [c]     → Clear queue
     - [x]     → Exit
            │
            ▼
        (back to IDLE or EXIT)
   ```

#### Changes

| File | Changes |
|------|---------|
| `src/ui/repl/ratatui_impl.rs` | Add SIGINT handling, interrupt overlay rendering |
| `src/ui/repl/renderer.rs` | Add `render_interrupt_overlay()` |
| `src/ui/app_state.rs` | Update `ReplState` with interrupt state |

#### Acceptance Criteria
- [ ] Ctrl+C interrupts running agent
- [ ] Pending messages visible in overlay
- [ ] Can resume, continue, clear, or exit

---

### Phase 5: Snapshot Tests for Interrupt UI

**Goal**: Add automated snapshot tests to verify interrupt UI renders correctly.

#### Test Categories

1. **Interrupt UI Tests** (snapshots):
   - Interrupted state with empty queue
   - Interrupted state with pending messages
   - Queue display (1, 2, 5+ messages)
   - Option hints visible

2. **Integration Tests** (with mock agent):
   - Message queue processing order
   - Context preservation across messages
   - Interrupt and resume flow
   - Clear queue behavior

#### Mock Agent for Integration Tests
```rust
use mockall::mock;

mock! {
    pub Agent {
        async fn prompt(&self, msg: &str) -> String;
        async fn prompt_with_history(&self, msg: &str, history: &mut Vec<Message>) -> String;
    }
}

#[tokio::test]
async fn test_queue_processing_order() { /* ... */ }

#[tokio::test]
async fn test_interrupt_preserves_context() { /* ... */ }
```

#### Changes

| File | Changes |
|------|---------|
| `tests/repl_snapshots.rs` | Add interrupt UI snapshot tests |
| `tests/repl_integration.rs` | Integration tests with mock agent |
| `tests/snapshots/*.snap` | New snapshot files (auto-generated) |
| `Cargo.toml` | Add `mockall` dev-dependency |

#### Running Tests
```bash
# Run all REPL tests
cargo test --test repl_snapshots --test repl_integration

# Update snapshots
INSTA_UPDATE=always cargo test
```

#### Acceptance Criteria
- [ ] Interrupt overlay renders correctly in all states
- [ ] Mock agent integration tests work
- [ ] Queue processing verified

---

### Phase 6: Message Injection (Interleave)

**Goal**: Allow injecting messages that become part of the agent's context in the next call.

#### Technical Approach

**Two injection modes**:

1. **Queue while processing** (type and press Enter):
   - Message goes to pending queue
   - Processed after current task completes
   - User can type while agent is working

2. **Interrupt + Inject** (Ctrl+C then type):
   - Cancels current task
   - Type message
   - Continues with queued message

**Context handling**:
```
When interrupted:
1. Current chat_history is preserved (don't clear!)
2. Injected message added to queue
3. On continue: processes queue in order
4. Each message sees full context from previous turns
```

#### Changes

| File | Changes |
|------|---------|
| `src/ui/repl/renderer.rs` | Show queue status in status bar, "Queued" feedback |
| `src/lib.rs` (AgentRunner) | `process_pending()`, context preservation |

#### Acceptance Criteria
- [ ] Can queue messages while agent running
- [ ] Queued messages process in order
- [ ] Context preserved across messages

---

### Phase 7: TUI Parity (Optional)

**Goal**: Apply interrupt/queue patterns to full TUI for consistency across UIs.

#### Changes

| File | Changes |
|------|---------|
| `src/ui/tui/tui_impl.rs` | Add interrupt handling similar to REPL |
| `src/ui/tui/input_handler.rs` | Map Ctrl+C to interrupt |

**Note**: This phase is optional if the interrupt behavior is deemed TUI-specific.

#### Acceptance Criteria
- [ ] Ctrl+C in TUI triggers interrupt mode
- [ ] Same options available (continue, resume, clear, exit)
- [ ] Consistent behavior with REPL

---

## Data Flow (After Refactor)

```
┌─────────────────────────────────────────────────────────────────┐
│                         User Input                              │
│  - Normal message: queue if busy, send if idle                 │
│  - Ctrl+C: cancel + show queue                                  │
│  - Options: resume, continue, clear                             │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                      UiAction Channel                           │
│  - SendMessage(String)                                          │
│  - CancelCurrent                                                │
│  - InterruptAndInject(String)                                    │
│  - InterruptResume                                              │
│  - InterruptContinue                                             │
│  - InterruptClearQueue                                          │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                     AgentRunner (Controller)                    │
│  - is_running: Arc<AtomicBool>                                 │
│  - is_interrupted: Arc<AtomicBool>                             │
│  - pending_queue: Arc<Mutex<Vec<String>>>                       │
│  - current_task: Option<JoinHandle>                            │
│                                                                 │
│  State Machine:                                                 │
│  ┌──────────┐    send_msg    ┌──────────────┐                 │
│  │  IDLE    │ ────────────► │  PROCESSING  │                 │
│  └──────────┘               └──────────────┘                 │
│       ▲                           │                            │
│       │ ctrl+c                    │ done                       │
│       └───────────────────────────┘                            │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                   StateManager (Model)                          │
│  - Broadcasts state to all Views                                │
│  - ReplState: is_processing, pending_queue, is_interrupted     │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                    Views (ratatui)                              │
│  - REPL View: Simplified layout + interrupt overlay             │
│  - TUI View: Full layout (optional parity)                     │
└─────────────────────────────────────────────────────────────────┘
```

---

## Files to Modify

### Core Implementation

| File | Changes |
|------|---------|
| `src/ui/repl/ratatui_impl.rs` | New REPL implementation using ratatui |
| `src/ui/repl/renderer.rs` | REPL-specific rendering components |
| `src/ui/app_state.rs` | Add `ReplState` struct |
| `src/ui/ui_trait.rs` | Add interrupt-related UiAction variants |
| `src/lib.rs` | Add task management to AgentRunner |
| `src/ui/tui/tui_impl.rs` | (Optional) Add interrupt patterns to TUI |

### Testing

| File | Changes |
|------|---------|
| `tests/repl_snapshots.rs` | Snapshot tests for REPL rendering |
| `tests/repl_integration.rs` | Integration tests with mock agent |
| `tests/snapshots/*.snap` | Snapshot files (auto-generated) |
| `Cargo.toml` | Add `insta` and `mockall` dev-dependencies |

---

## New Dependencies

| Dependency | Purpose | Type |
|------------|---------|------|
| `ratatui` | Already used by TUI | existing |
| `crossterm` | Already used by TUI (via ratatui) | existing |
| `tokio::task::JoinHandle` | Task management | existing |
| `tokio::signal::ctrl_c()` | SIGINT handling | existing |
| `insta` | Snapshot testing | **new dev-dependency** |
| `mockall` | Mock agent for integration tests | **new dev-dependency** |

---

## Testing Strategy

### Testing-First Approach
Each implementation phase is followed by tests to verify correctness:
1. **Phase 1** → **Phase 2**: Snapshot tests for REPL foundation
2. **Phase 4** → **Phase 5**: Snapshot tests for interrupt UI + integration tests

### Running Tests
```bash
# Run all snapshot tests
cargo test --test repl_snapshots

# Run integration tests
cargo test --test repl_integration

# Run all REPL tests
cargo test --test repl_snapshots --test repl_integration

# Update snapshots (when intentionally changing output)
INSTA_UPDATE=always cargo test
```

**Key reference**: See `~/workz/tuitest` for complete working example of:
- `ratatui::backend::TestBackend` for in-memory rendering
- `insta` for snapshot assertions
- Test helpers for rendering to string

---

## Zen of Engineering Analysis

### What can go wrong?
- Multiple rapid Ctrl+C presses — mitigated by debounce
- State inconsistency if task abort leaves partial data — mitigated by preserving context
- Code duplication between TUI and REPL renderers — mitigated by extracting shared components

### What can be removed?
- Legacy `run()` method in AgentRunner (non-MVC mode)
- Old `repl_impl.rs` (replaced by ratatui version)

### What will confuse people?
- Task abort semantics with Rig (may leave partial state in LLM provider)
- Two REPL implementations during transition

### What is superfluous?
- Complex queue management (simple Vec is fine)
- Streaming output (out of scope)

### One thing to keep in mind
**The agent's context (chat_history) must survive cancellation.** Don't clear it — the user expects to resume or continue with context intact.

---

## Implementation Order

1. **Phase 1** (ratatui REPL) — foundation with cross-platform support
2. **Phase 2** (Snapshot tests) — verify REPL foundation works
3. **Phase 3** (Async task) — foundation for cancel/queue
4. **Phase 4** (SIGINT) — immediate user value
5. **Phase 5** (Snapshot tests) — verify interrupt UI works
6. **Phase 6** (Message injection) — complete the story
7. **Phase 7** (TUI parity) — consistency across UIs (optional)

Each phase is independently deployable and testable.

---

## Future Considerations (Out of Scope)

- Streaming output to REPL (agent prints as it thinks)
- Multiple concurrent agent sessions
- Persistence of pending queue across restarts
- WebSocket-based remote REPL
