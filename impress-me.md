# Stop the Bullshit: A Plan for Real Agent Interruption

## The Problem

Currently, `/stop` is theater. The `AgentRunner.run_loop()` is a **single loop** that receives `UiAction` messages and processes them sequentially. When `process_message()` is called, it blocks until done. If the user presses `/stop` while the agent is running, we set `session_hook.request_stop()` but we're stuck in `process_message()` — the stop flag is only checked at LLM boundaries in `on_completion_response()`, which never fires because we're blocked in the tool-call loop.

## The Zen Solution

**Two loops inside `AgentRunner`, communicating via a channel.**

Simplicity. The MVC architecture is already correct. We just need to split the single loop into two. We use the existing `StateManager` (the Model in MVC) to track running state — it already broadcasts to all UIs.

```
┌─────────────────────────────────────────────────────────────────┐
│  AgentRunner.run()                                               │
│                                                                  │
│  ┌─────────────────────────┐     ┌─────────────────────────────┐│
│  │  Event Loop             │     │  Agent Loop                  ││
│  │  (tokio::spawn'd)       │     │  (tokio::spawn'd)           ││
│  │                         │     │                              ││
│  │  Receives UiAction      │────▶│  Reads from message_queue    ││
│  │  from View              │     │  Calls process_message()      ││
│  │                         │     │  Sends completion_tx         ││
│  │  On /stop:              │     │                              ││
│  │    - Set stop flag      │     │  At LLM boundary:            ││
│  │    - Queue /stop        │     │    - SessionHook checks flag ││
│  │                         │     │    - Returns terminate()     ││
│  │  On new message:        │     │    - process_message returns ││
│  │    - Queue it           │     │    - Read next from queue    ││
│  │    - Set stop flag      │     │                              ││
│  └─────────────────────────┘     └─────────────────────────────┘│
│          │                                    │                  │
│          │           ┌─────────────────────────┘                  │
│          │           │                                            │
│          │    ┌──────┴───────┐                                   │
│          └───▶│  message_queue │◀───────────────────────────────┘
│               │  (tokio channel)│
└─────────────────────────────────────────────────────────────────┘
```

## The Flow

### Normal message (when agent is idle):
1. Event loop receives `UiAction::SendMessage("hello")`
2. Queues `"hello"` to message channel
3. Agent loop reads `"hello"` from channel
4. Agent loop sets `state_manager.set_running(true)`
5. Agent loop calls `process_message("hello")`
6. At LLM response, sends completion notification
7. Agent loop sets `state_manager.set_running(false)`
8. Event loop receives notification, displays response

### User presses `/stop` while agent is running:
1. Event loop receives `UiAction::RequestStop` (or `/stop` command)
2. **Only if `state_manager.is_running()`**: Sets `session_hook.request_stop()`
3. Queues `StopMarker` to message channel
4. Agent loop is at LLM boundary, SessionHook sees stop flag
5. SessionHook returns `HookAction::terminate("stop")`
6. `process_message()` catches `PromptError::PromptCancelled`
7. Agent loop sets `state_manager.set_running(false)`
8. Agent loop reads next from queue: `StopMarker`
9. Agent loop handles stop (prints "[Agent stopped by user]")
10. Event loop receives notification

### User sends new message while agent is running:
1. Event loop receives `UiAction::SendMessage("new msg")`
2. **Only if `state_manager.is_running()`**: Sets `session_hook.request_stop()` (to interrupt current)
3. Queues `"new msg"` to message channel
4. Agent loop is interrupted at LLM boundary
5. `process_message()` catches the cancellation
6. Agent loop sets `state_manager.set_running(false)`
7. Agent loop reads next from queue: `"new msg"`
8. Agent loop sets `state_manager.set_running(true)`
9. Agent loop calls `process_message("new msg")` - fresh start
10. Success, sets `state_manager.set_running(false)`, sends completion notification
11. Event loop displays response

## Architecture Changes

### StateManager changes (`src/ui/state_manager.rs`)

Rename `set_loading` to `set_running` and add `is_running`:

```rust
impl StateManager {
    /// Set whether the agent is currently running (processing a message)
    pub fn set_running(&self, running: bool) {
        let mut state = self.state.write().unwrap();
        state.agent_running = running;
        drop(state);
        self.broadcast();
    }
    
    /// Check if the agent is currently running
    pub fn is_running(&self) -> bool {
        self.state.read().unwrap().agent_running
    }
}
```

Update `AppState` struct to use `agent_running` instead of `is_loading`.

### Modify `src/lib.rs` - Split `run_loop()` into two loops

```rust
/// Message types for internal queue between event loop and agent loop
enum QueueMessage {
    UserMessage(String),
    Command(String),
    StopMarker,  // Signals that stop was requested
}

impl AgentRunner {
    /// New unified entry point - spawns two loops internally
    pub async fn run(&mut self, action_receiver: mpsc::UnboundedReceiver<UiAction>) {
        // Channel between event loop and agent loop
        let (msg_tx, msg_rx) = tokio::sync::mpsc::channel::<QueueMessage>(32);
        
        // Completion notifications back to event loop
        let (completion_tx, _completion_rx) = tokio::sync::broadcast::channel::<CompletionResult>(8);
        
        // Shared chat history (needed by both loops)
        let chat_history = Arc::new(tokio::sync::Mutex::new(Vec::<Message>::new()));
        
        // Spawn the two loops
        let event_handle = tokio::spawn({
            let msg_tx = msg_tx.clone();
            let completion_tx = completion_tx.clone();
            let chat_history = chat_history.clone();
            
            async move {
                Self::event_loop(
                    action_receiver,
                    msg_tx,
                    completion_tx,
                    chat_history,
                ).await;
            }
        });
        
        let agent_handle = tokio::spawn({
            let msg_rx = tokio::sync::Mutex::new(msg_rx);
            let completion_tx = completion_tx.clone();
            let chat_history = chat_history.clone();
            
            async move {
                Self::agent_loop(
                    msg_rx,
                    completion_tx,
                    chat_history,
                ).await;
            }
        });
        
        // Wait for event loop to exit (View closed)
        event_handle.await.ok();
        agent_handle.abort();
    }
    
    /// Event loop - receives UiActions from View
    async fn event_loop(
        mut action_receiver: mpsc::UnboundedReceiver<UiAction>,
        msg_tx: tokio::sync::mpsc::Sender<QueueMessage>,
        completion_tx: tokio::sync::broadcast::Sender<CompletionResult>,
        chat_history: Arc<tokio::sync::Mutex<Vec<Message>>>,
    ) {
        // Subscribe to completion notifications
        let _completion_rx = completion_tx.subscribe();
        
        // Initialize conversation (moved from run_loop)
        // ... conversation setup ...
        
        while let Some(action) = action_receiver.recv().await {
            match action {
                UiAction::SendMessage(msg) => {
                    // Save to conversation manager
                    if let Some(ref cm) = self.conversation_manager {
                        let _ = cm.lock().unwrap().add_user_message(msg.clone());
                    }
                    // Update state manager for live rendering
                    if let Some(ref sm) = self.state_manager {
                        sm.update_chat(ChatMessage::user(msg.clone()));
                    }
                    
                    // If agent is running, interrupt it first
                    if self.state_manager.as_ref().map_or(false, |sm| sm.is_running()) {
                        self.session_hook.request_stop();
                    }
                    
                    // Queue the new message for the agent
                    msg_tx.send(QueueMessage::UserMessage(msg)).await.ok();
                }
                
                UiAction::ExecuteCommand(cmd) => {
                    if cmd == "/stop" {
                        // Only stop if agent is actually running
                        if self.state_manager.as_ref().map_or(false, |sm| sm.is_running()) {
                            self.session_hook.request_stop();
                            msg_tx.send(QueueMessage::StopMarker).await.ok();
                            println!("[Stop requested...]");
                        }
                    } else {
                        // Queue command for agent loop
                        msg_tx.send(QueueMessage::Command(cmd)).await.ok();
                    }
                }
                
                UiAction::RequestStop => {
                    // Only stop if agent is actually running
                    if self.state_manager.as_ref().map_or(false, |sm| sm.is_running()) {
                        self.session_hook.request_stop();
                        msg_tx.send(QueueMessage::StopMarker).await.ok();
                        println!("[Stop requested...]");
                    }
                }
                
                UiAction::Exit => {
                    break;
                }
            }
        }
    }
    
    /// Agent loop - processes messages from event loop
    async fn agent_loop(
        msg_rx: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<QueueMessage>>,
        completion_tx: tokio::sync::broadcast::Sender<CompletionResult>,
        chat_history: Arc<tokio::sync::Mutex<Vec<Message>>>,
    ) {
        loop {
            // Wait for a message
            let msg = msg_rx.lock().await.recv().await;
            
            match msg {
                Some(QueueMessage::UserMessage(content)) => {
                    // Mark as running via StateManager (broadcasts to all UIs)
                    if let Some(ref sm) = self.state_manager {
                        sm.set_running(true);
                    }
                    
                    let result = self.process_message_internal(&content, chat_history.clone()).await;
                    
                    // Mark as done
                    if let Some(ref sm) = self.state_manager {
                        sm.set_running(false);
                    }
                    
                    // Send completion notification
                    completion_tx.send(result).ok();
                }
                
                Some(QueueMessage::Command(cmd)) => {
                    if let Some(ref sm) = self.state_manager {
                        sm.set_running(true);
                    }
                    self.process_command_internal(&cmd, chat_history.clone()).await;
                    if let Some(ref sm) = self.state_manager {
                        sm.set_running(false);
                    }
                    completion_tx.send(CompletionResult::CommandDone).ok();
                }
                
                Some(QueueMessage::StopMarker) => {
                    // This is just an acknowledgment that stop was requested
                    // The actual stopping happened in process_message_internal
                    println!("[Agent stopped by user]");
                    completion_tx.send(CompletionResult::Stopped).ok();
                }
                
                None => {
                    // Channel closed, exit
                    break;
                }
            }
        }
    }
    
    /// Internal process_message - returns CompletionResult instead of handling directly
    async fn process_message_internal(
        &self,
        msg: &str,
        chat_history: Arc<tokio::sync::Mutex<Vec<Message>>>,
    ) -> CompletionResult {
        let mut retry_count = 0;
        let mut current_msg = msg.to_string();
        
        loop {
            // Context compaction check
            if let Some(ref mut cm) = self.context_manager {
                let mut history = chat_history.lock().await;
                if cm.needs_compaction(&history) {
                    println!("[Context approaching limit, compacting...]");
                    // ... compaction logic ...
                }
            }
            
            // Call the agent
            let mut history = chat_history.lock().await;
            let result = self.agent
                .as_ref()
                .prompt_with_history(&current_msg, &mut history)
                .await;
            
            match result {
                Ok(response) => {
                    return CompletionResult::Success(response);
                }
                
                Err(PromptError::PromptCancelled { reason, .. }) => {
                    // On stop, just return Stopped. Let the loop handle the StopMarker.
                    if reason == "stop" {
                        return CompletionResult::Stopped;
                    }
                    // Other cancellations (compact, etc) - loop continues
                }
                
                Err(_) => {
                    if retry_count == self.config.retry().max_retries {
                        return CompletionResult::Error("Max retries exceeded".to_string());
                    }
                    retry_count += 1;
                }
            }
        }
    }
    
    /// Internal process_command
    async fn process_command_internal(
        &self,
        cmd: &str,
        chat_history: Arc<tokio::sync::Mutex<Vec<Message>>>,
    ) {
        let cmd_lower = cmd.to_lowercase();
        
        match cmd_lower.as_str() {
            "/stats" => self.print_stats(),
            "/reset" => { self.reset_stats(); println!("Stats reset.\n"); }
            "/context" => self.print_context_status(),
            "/compact" => {
                let mut history = chat_history.lock().await;
                self.force_compact(&mut history).await;
            }
            // ... other commands ...
            _ => {
                // Unknown command - treat as message
                let _result = self.process_message_internal(cmd, chat_history).await;
            }
        }
    }
}

enum CompletionResult {
    Success(String),
    Stopped,
    Error(String),
    CommandDone,
}
```

### Simplify `src/hooks/session_hook.rs`

Remove the injection nonsense. Just do stop.

```rust
impl SessionHook {
    // Remove:
    // - pending_message field
    // - queue_message() method
    // - __INJECT__: termination logic
    
    // Keep:
    // - stop_requested AtomicBool
    // - request_stop() method
}

impl<M: CompletionModel> PromptHook<M> for SessionHook {
    async fn on_completion_response(&self, ...) -> HookAction {
        // ... emit event ...
        
        // Stop only
        if self.stop_requested.load(Ordering::SeqCst) {
            self.stop_requested.store(false, Ordering::SeqCst);
            return HookAction::terminate("stop");
        }
        
        HookAction::Continue
    }
}
```

### Modify `src/main.rs`

The spawn pattern already exists! Just needs to call the new `run()` method:

```rust
#[tokio::main]
async fn main() -> Result<()> {
    // ... setup ...
    
    let mut runner = AgentRunner::new(...);
    
    // This now spawns two internal loops
    runner.run(action_receiver).await;
    
    Ok(())
}
```

## File Map After Changes

```
src/
├── lib.rs                  # AgentRunner with TWO loops (event_loop + agent_loop)
├── main.rs                 # Unchanged (or minimal change to call run())
├── hooks/
│   └── session_hook.rs     # Simplified - kill the injection magic
└── ui/
    └── state_manager.rs    # Rename set_loading to set_running, add is_running
```

**That's it.** Three files changed, one file simplified.

## Benefits

1. **Stop actually works** - Interrupt at next LLM boundary
2. **Queue new messages** - If user types while agent is running, the new message queues up and runs after stop
3. **No magic strings** - Kill `__INJECT__:`, kill `pending_message`, kill injection logic
4. **Minimal changes** - Only `lib.rs`, `session_hook.rs`, and `state_manager.rs` need updating
5. **UI sees running state** - StateManager broadcasts `is_running` to all UIs, which can display "[Agent running...]" or similar
6. **Testable** - Each loop can be tested independently

## Implementation Order

1. **Phase 1**: Update `StateManager` - rename `set_loading` to `set_running`, add `is_running()`
2. **Phase 2**: Simplify `SessionHook` - remove injection, remove `pending_message`, keep only stop
3. **Phase 3**: Split `run_loop()` in `lib.rs` into `event_loop()` and `agent_loop()`
4. **Phase 4**: Add `QueueMessage` enum and internal channels
5. **Phase 5**: Update `process_message_internal()` to use shared history
6. **Phase 6**: Update `main.rs` to call `runner.run()` instead of `runner.run_loop()`
7. **Phase 7**: Test stop during tool execution, during LLM think

## Testing

```rust
#[tokio::test]
async fn stop_works_during_tool_execution() {
    // Agent is in middle of bash tool
    // User sends /stop
    // Assert: stops at next LLM boundary
}

#[tokio::test]
async fn message_queues_after_stop() {
    // Agent is running
    // User presses /stop
    // User types new message
    // Assert: new message runs after stop acknowledged
}

#[tokio::test]
async fn immediate_message_interrupts() {
    // Agent is running
    // User types new message
    // Assert: agent stops, new message runs
}
```
