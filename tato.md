# PeakBot End-to-End Testing: Glory Plan

**TL;DR**  
We need to build a mock LLM + test harness to verify the complete flow: user input → agent processing → state updates → conversation persistence. The UI is the only thing missing.

---

## 1. What to Test

### 1.1 The Critical Paths

```
User Input
    │
    ▼
┌────────────────────────────┐
│   AgentRunner              │
│   process_message_internal │
└───────────┬────────────────┘
            │
    ┌───────┼───────┐
    ▼       ▼       ▼
┌────────┐ ┌──────┐ ┌─────────────────────┐
│ DynAgent│ │State │ │ ConversationManager │
│(mock)  │ │Mgr   │ │ (persistence)       │
└────────┘ └──────┘ └─────────────────────┘
    │       │              │
    ▼       ▼              ▼
┌────────┐ ┌────────┐ ┌──────────────┐
│ Session │ │ AppState│ │ JSON files  │
│ Hook   │ │(verify)│ │ (verify)     │
│(events)│ │        │ │              │
└────────┘ └────────┘ └──────────────┘
```

### 1.2 Test Scenarios

| Scenario | Input | Mock Response | Verification |
|----------|-------|---------------|--------------|
| **simple_message_roundtrip** | "Hello" | "Hi there!" | StateManager: 1 user msg, 1 agent msg |
| **tool_call_updates_state** | "Add todo: Fix bug" | `{"tool":"todo","args":{...}}` | StateManager: 1 todo item added |
| **todo_status_cycle** | "Complete task #1" | `{"tool":"todo","args":{...}}` | StateManager: task #1 = completed |
| **stats_accumulate** | 3 messages | 3 responses with tokens | StateManager: 3 API calls, tokens summed |
| **conversation_persists** | "Write code" | "Done" | Conversation file exists, loads correctly |
| **event_emission** | "Hello" | "Hi" | SessionHook: CompletionResponse event emitted |
| **context_compaction** | 50 long msgs | summaries | Context compacted when threshold hit |
| **stop_request** | /stop | (interrupted mid-turn) | StateManager: is_running = false |

---

## 2. Architecture

### 2.1 File Structure

```
tests/
├── integration.rs                 # Test module entry point
├── harness/
│   ├── mod.rs
│   └── test_harness.rs           # TestHarness orchestrator
├── mock/
│   ├── mod.rs
│   ├── completion_model.rs       # MockCompletionModel
│   └── response.rs              # Response builders
├── storage/
│   ├── mod.rs
│   ├── storage_trait.rs         # ConversationStorage trait
│   └── in_memory.rs             # InMemoryStorage for tests
└── scenarios/
    ├── mod.rs
    ├── message_roundtrip.rs     # Message flow tests
    ├── stats_tests.rs           # Cost tracking tests
    └── tool_tests.rs            # Tool functionality tests
```

### 2.2 Mock Layer

#### MockCompletionModel

```rust
// A configurable mock that implements rig::completion::CompletionModel
pub struct MockCompletionModel {
    responses: VecDeque<MockResponse>,
    tool_calls: VecDeque<ToolCall>,
}

impl MockCompletionModel {
    pub fn new() -> Self;
    
    // Queue a text response
    pub fn add_text_response(&mut self, text: &str) -> &mut Self;
    
    // Queue a tool call response
    pub fn add_tool_call(&mut self, tool_name: &str, args: serde_json::Value) -> &mut Self;
    
    // Queue an error
    pub fn add_error(&mut self, error: PromptError) -> &mut Self;
    
    // Set token usage for responses
    pub fn set_token_usage(&mut self, input: u64, output: u64) -> &mut Self;
}
```

**Key insight**: The mock must integrate with Rig's agent system. We create the mock model, pass it to a test agent, and the agent's `prompt_with_history()` will use our mock.

#### Direct Tool Testing (No Mock Needed)

For the **TodoTool**, we test it directly—no mock wrapper needed. The TodoTool is a thin controller that delegates to StateManager. We just:

1. Create a real `TodoTool::new(state_manager.clone())`
2. Call it with test arguments via `tool.call(args)`
3. Verify StateManager state changed

```rust
#[tokio::test]
async fn test_todo_adds_item() {
    // Setup
    let state_manager = Arc::new(StateManager::new());
    let todo_tool = TodoTool::new(state_manager.clone());
    
    // Call the REAL tool
    let result = todo_tool.call(TodoArgs {
        thought: "Adding a test task".into(),
        action: "add".into(),
        tasks: Some(vec!["Fix bug".into()]),
        status: None,
        task_id: None,
    }).await;
    
    // Verify state changed
    assert!(result.is_ok());
    assert_eq!(state_manager.get_state().todo.items.len(), 1);
    assert_eq!(state_manager.get_state().todo.items[0].task, "Fix bug");
}
```

**Why this is better**:
- Tests the real code path (no mocking = no mock bugs)
- Exercises `TodoTool::call()` end-to-end
- Validates the StateManager integration
- Same approach for any state-modifying tool

### 2.3 Test Harness

```rust
pub struct TestHarness {
    /// State manager for tracking state changes
    pub state_manager: Arc<StateManager>,
    /// Mock completion model for simulating LLM responses
    pub mock_model: MockCompletionModel,
    /// Mock agent for testing
    pub agent: Agent<MockCompletionModel, ()>,
    /// Optional event receiver for collecting agent events
    pub event_receiver: Option<mpsc::UnboundedReceiver<AgentEvent>>,
    /// Conversation storage for persistence tests
    pub storage: Arc<InMemoryStorage>,
}

impl TestHarness {
    pub fn new() -> Self;
    
    /// Create with custom system prompt
    pub fn with_system_prompt(preamble: &str) -> Self;
    
    /// Add a mock response to the queue
    pub fn add_response(&self, response: MockResponse);
    
    /// Add multiple mock responses
    pub fn add_responses(&self, responses: impl IntoIterator<Item = MockResponse>);
    
    /// Run a message through the agent
    pub async fn run_message(&self, message: &str) -> String;
    
    /// Run a message with history
    pub async fn run_message_with_history(&self, message: &str, history: &mut Vec<Message>) -> String;
    
    /// Get current state snapshot
    pub fn get_state(&self) -> AppState;
    
    /// Get todo list
    pub fn get_todos(&self) -> Vec<TodoItem>;
    
    /// Get stats
    pub fn get_stats(&self) -> SessionStats;
    
    /// Check remaining responses
    pub fn has_remaining_responses(&self) -> bool;
    pub fn remaining_responses(&self) -> usize;
}

## 3. Key Architectural Decisions

### 3.1 Direct Tool Testing (No Mocking!)

For **state-modifying tools like TodoTool**, we test them **directly**—no mock wrapper needed.

**Why this is BETTER:**
- Tests the REAL code path (no mocking = no mock bugs)
- Exercises `TodoTool::call()` end-to-end  
- Validates the StateManager integration
- Same approach for any state-modifying tool

**How it works:**
```rust
#[tokio::test]
async fn test_todo_adds_item() {
    // Setup: real StateManager + real TodoTool (no mocks!)
    let state_manager = Arc::new(StateManager::new());
    let todo_tool = TodoTool::new(state_manager.clone());
    
    // Call the ACTUAL tool
    let result = todo_tool.call(TodoArgs {
        thought: "Adding a test task".into(),
        action: "add".into(),
        tasks: Some(vec!["Fix bug".into()]),
        status: None,
        task_id: None,
    }).await;
    
    // Verify StateManager state changed directly
    assert!(result.is_ok());
    assert_eq!(state_manager.get_state().todo.items.len(), 1);
    assert_eq!(state_manager.get_state().todo.items[0].task, "Fix bug");
}
```

The mock layer (`MockCompletionModel`) is only for testing **agent behavior** (model responses, tool call routing, context management). For tools that modify state, we call them directly and verify state changes.

### 3.2 Storage Abstraction

We abstract conversation storage behind a trait, enabling:
- **Testing**: In-memory storage for fast, isolated tests (no temp files!)
- **Production**: File-based storage (current)
- **Future**: Database storage for multi-instance deployments

This is a **strategic decision**—it future-proofs the architecture and makes tests faster and more reliable.

#### Storage Trait

```rust
use uuid::Uuid;

/// Abstract storage for conversations - implement this for any backend
pub trait ConversationStorage: Send + Sync {
    /// Save a conversation
    fn save(&self, conversation: &Conversation) -> Result<()>;
    
    /// Load a conversation by ID
    fn load(&self, id: Uuid) -> Result<Conversation>;
    
    /// List all conversation summaries
    fn list(&self) -> Result<Vec<ConversationSummary>>;
    
    /// Delete a conversation by ID
    fn delete(&self, id: Uuid) -> Result<()>;
}
```

#### Implementations

**InMemoryStorage** (for testing):
```rust
pub struct InMemoryStorage {
    conversations: Arc<Mutex<HashMap<Uuid, Conversation>>>,
}

// Tests use this - no temp files, no cleanup, instant
impl ConversationStorage for InMemoryStorage { ... }
```

**FileStorage** (current production):
```rust
pub struct FileStorage {
    storage_dir: PathBuf,
}

// Production uses this - persists to disk
impl ConversationStorage for FileStorage { ... }
```

**DatabaseStorage** (future):
```rust
pub struct DatabaseStorage {
    pool: sqlx::PgPool,
}

// Future: multi-instance, persistent, queryable
impl ConversationStorage for DatabaseStorage { ... }
```

#### Updated ConversationManager

```rust
// Generic over storage backend - defaults to FileStorage
pub struct ConversationManager<S: ConversationStorage = FileStorage> {
    storage: S,
    current: Option<Uuid>,
}

// Type alias for backward compatibility
pub type FileConversationManager = ConversationManager<FileStorage>;
```

**Benefits:**
- Tests run 10x faster (no disk I/O)
- No temp file cleanup needed
- Tests are fully isolated
- Easy to swap backends without changing tests

---

## 3. Implementation Phases

### Phase 1a: Mock Layer

**Goal**: Create a MockCompletionModel that Rig's agent can use for full roundtrip tests.

**Files to create**:
- `tests/integration/mock/completion_model.rs`
- `tests/integration/mock/response.rs`
- `tests/integration/mock/mod.rs`

**MockCompletionModel implementation**:

```rust
pub struct MockCompletionModel {
    responses: VecDeque<MockResponse>,
    default_usage: Usage,
}

impl CompletionModel for MockCompletionModel {
    type Response = MockResponse;
    
    async fn completion(
        &self,
        _prompt: &Message,
        _history: &[Message],
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        // Pop next response or return error if queue empty
    }
}
```

**Testing approach**: Create agent with MockCompletionModel, call `prompt_with_history()`, verify mock responses returned.

### Phase 1b: Storage Abstraction Refactor

**Goal**: Extract storage logic from ConversationManager into a trait, enabling different backends.

**Files to modify**:
- `src/conversation/manager.rs` — Add `ConversationStorage` trait, update ConversationManager to be generic
- `src/conversation/storage/` (new directory):
  - `storage_trait.rs` — `ConversationStorage` trait definition
  - `file_storage.rs` — Current file-based implementation
  - `in_memory_storage.rs` — For testing

**Implementation**:

```rust
// src/conversation/storage_trait.rs
pub trait ConversationStorage: Send + Sync {
    fn save(&self, conversation: &Conversation) -> Result<()>;
    fn load(&self, id: Uuid) -> Result<Conversation>;
    fn list(&self) -> Result<Vec<ConversationSummary>>;
    fn delete(&self, id: Uuid) -> Result<()>;
}

// Refactored ConversationManager
pub struct ConversationManager<S: ConversationStorage> {
    storage: S,
    current: Option<Uuid>,
    // ... existing fields
}

impl<S: ConversationStorage> ConversationManager<S> {
    pub fn new(storage: S, config: ConversationManagerConfig) -> Self;
    
    // Methods now delegate to self.storage
    pub fn save(&self) -> Result<()> {
        if let Some(id) = self.current {
            let conv = self.storage.load(id)?;
            self.storage.save(&conv)
        } else {
            Ok(())
        }
    }
}
```

**Migration**: Default `ConversationManager` to `FileStorage` for backward compatibility. Add type alias:

```rust
pub type FileConversationManager = ConversationManager<FileStorage>;
```

### Phase 2: Test Harness

**Goal**: Build a reusable harness for all scenarios.

**Files to create**:
- `tests/integration/harness/state_observer.rs`
- `tests/integration/harness/test_harness.rs`
- `tests/integration/harness/mod.rs`

**Key methods**:

```rust
impl TestHarness {
    /// Set up minimal environment (StateManager, temp dirs)
    pub fn new() -> Self;
    
    /// Add a mock response (text or tool call)
    pub fn queue_response(&mut self, response: MockResponse) -> &mut Self;
    
    /// Run a user message through the agent
    pub async fn process_message(&mut self, msg: &str) -> AgentResult;
    
    /// Call a tool directly (for testing TodoTool, BashTool, etc.)
    pub async fn call_tool<T: Tool>(&self, tool: &T, args: serde_json::Value) -> String;
}
```

### Phase 3: Core Scenarios

**Scenario 1: simple_message_roundtrip**

```
Test: User sends "Hello" → Mock returns "Hi!" → State verified

Steps:
1. Create TestHarness
2. Queue mock response: text "Hi!"
3. Run message "Hello"
4. Assert:
   - state_manager.get_state().chat.messages.len() == 2
   - state_manager.get_state().chat.messages[0] == user("Hello")
   - state_manager.get_state().chat.messages[1] == agent("Hi!")
```

**Scenario 2: tool_call_updates_state**

```
Test: User asks to add todo → Mock returns todo tool call → Todo verified

Steps:
1. Create TestHarness with TodoTool wired to StateManager
2. Queue mock response: tool_call("todo", {"action":"add","tasks":["Fix bug"]})
3. Queue mock response: text "Added task #1: Fix bug"
4. Run message "Add a todo to fix the bug"
5. Assert:
   - state_manager.get_state().todo.items.len() == 1
   - state_manager.get_state().todo.items[0].task == "Fix bug"
   - state_manager.get_state().todo.items[0].status == TodoStatus::Pending
```

**Scenario 3: stats_accumulate**

```
Test: 3 messages → Stats accumulate correctly

Steps:
1. Create TestHarness
2. Set mock token usage: input=100, output=50
3. Queue 3 responses with usage
4. Run 3 messages
5. Assert:
   - state_manager.get_state().stats.total_api_calls == 3
   - state_manager.get_state().stats.total_input_tokens == 100 (last)
   - state_manager.get_state().stats.total_output_tokens == 50 (last)
```

**Scenario 4: conversation_persists**

```
Test: Message exchange → Saved to disk → Loaded → Same content

Steps:
1. Create TestHarness with ConversationManager (temp dir)
2. Run message "Hello"
3. Verify conversation file exists in temp dir
4. Load conversation by ID
5. Assert:
   - conversation.messages.len() == 2
   - conversation.messages[0] == user("Hello")
   - conversation.messages[1] == agent("Hi!")
```

**Scenario 5: event_emission**

```
Test: Message → SessionHook emits events

Steps:
1. Create TestHarness with event receiver
2. Queue mock response: text "Hi!"
3. Run message "Hello"
4. Collect events
5. Assert:
   - events contains AgentEvent::CompletionRequest
   - events contains AgentEvent::CompletionResponse
   - events contains AgentEvent::ToolCall (if tool used)
```

### Phase 4: Edge Cases

**Scenario 6: context_compaction**

```
Test: Long conversation → Context threshold → Compaction

Steps:
1. Create TestHarness with ContextManager enabled
2. Set context threshold to 80%
3. Mock returns large usage (near threshold)
4. Run 50 messages (accumulate context)
5. Assert:
   - ContextManager.needs_compaction() triggered
   - history compacted to summary + recent messages
```

**Scenario 7: stop_request**

```
Test: /stop command → Agent interrupted

Steps:
1. Create TestHarness
2. Start agent processing (mock returns slow/no response)
3. Send /stop command
4. Assert:
   - state_manager.get_state().is_running == false
   - events contains stop-related events
```

---

## 4. Zen-Compliant Design Decisions

### 4.1 What We Include

✅ **Direct state observation** — Test harness directly accesses StateManager  
✅ **Minimal mock layer** — Only MockCompletionModel + MockTool  
✅ **One scenario per test** — No mega-tests, focused assertions  
✅ **Temporary directories** — Auto-cleanup, no leftover files  
✅ **Synchronous assertions** — Use `block_on()` for async tests  

### 4.2 What We Exclude

❌ **No UI rendering tests** — Already covered by `repl_tests.rs`  
❌ **No real API calls** — Only mocks  
❌ **No MCP server tests** — Would require external processes  
❌ **No multi-provider variations** — Mock is provider-agnostic  
❌ **No config file parsing** — Create Config struct directly  

### 4.3 Why This Approach

**"Simplicity is the key"**:  
Direct StateManager observation + mock agent is simpler than trying to mock Rig's internal agent loop or intercepting REPL commands.

**"Fewer pieces → fewer things that can go wrong"**:  
MockCompletionModel + TestHarness + StateObserver = 3 components. Each is focused.

**"Don't be too clever"**:  
We don't try to mock the entire agentic loop. We mock at the provider level and let Rig do its thing with our mock.

**"Make illegal states unrepresentable"**:  
Use typed `MockResponse` enum, not raw JSON. Invalid combinations rejected at compile time.

---

## 5. Dependencies

**No new dependencies needed**. We use:
- `tempfile` (already in Cargo.toml for tests)
- `tokio::test` (already available)
- Existing mock infrastructure if any

**Optional enhancement**: Add `pretty_assertions` for nicer diffs in test failures.

```toml
[dev-dependencies]
pretty_assertions = "1.4"
```

---

## 6. Verification

### 6.1 Run the tests

```bash
cargo test --test integration
```

### 6.2 Expected output

```
running 30 tests
tests/scenarios/message_roundtrip.rs::test_basic_roundtrip ... ok
tests/scenarios/message_roundtrip.rs::test_multi_turn_conversation ... ok
tests/scenarios/message_roundtrip.rs::test_system_prompt_preserved ... ok
tests/scenarios/tool_tests.rs::test_todo_tool_add ... ok
tests/scenarios/tool_tests.rs::test_todo_tool_update ... ok
tests/scenarios/stats_tests.rs::test_stats_tracking ... ok
tests/scenarios/stats_tests.rs::test_sequential_requests ... ok
tests/harness/test_harness.rs::tests::test_simple_message_roundtrip ... ok
tests/harness/test_harness.rs::tests::test_multiple_responses ... ok
tests/harness/test_harness.rs::tests::test_tool_call_response ... ok
...

test result: ok. 30 passed; 0 failed
```

### 6.3 CI Integration

Add to `.github/workflows/test.yml`:

```yaml
- name: Run integration tests
  run: cargo test --test integration -- --nocapture
```

---

## 7. Future Enhancements

Once this framework is in place, we can easily add:

1. **Benchmarking** — Measure token usage, response time
2. **Snapshot testing** — Compare state snapshots to expected JSON
3. **Fuzzing** — Random tool call sequences
4. **Load testing** — Many concurrent sessions
5. **Regression detection** — Compare runs across code changes

---

## 8. Summary

| Component | Files | Description |
|-----------|-------|-------------|
| Mock Layer | `tests/mock/` | MockCompletionModel for LLM simulation |
| Storage | `tests/storage/` | ConversationStorage trait + InMemoryStorage |
| Test Harness | `tests/harness/` | TestHarness for test orchestration |
| Scenarios | `tests/scenarios/` | message_roundtrip, stats, tool tests |

**Test Coverage**: 30 tests covering:
- Message roundtrips (basic, multi-turn, system prompt)
- Stats tracking and cost accumulation
- Todo tool operations (add, update, list, remove)
- Storage operations (save, load, clear)
- Error handling and edge cases

---

*For the glory of God—clean code, precise tests, and the satisfaction of knowing our system works.*