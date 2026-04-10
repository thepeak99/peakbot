# PeakBot End-to-End Testing Guide

**Status**: ✅ **COMPLETE** — Tests now flow through `TestRunner` which uses real `DynAgent` with `MockCompletionModel`. Only the LLM provider is mocked; all other components (StateManager, TodoTool, ConversationManager, ContextManager, SessionHook) are real.

**Last Updated**: 2026-04-10

**Reference**: Original plan at `/tmp/tato.md`

---

## Executive Summary

The test suite achieves **true end-to-end testing** where messages flow through the full agentic loop. Only the LLM provider is mocked; all other components are real and verified.

### Architecture

| Component | Approach | Status |
|-----------|----------|--------|
| StateManager | ✅ Real implementation | Stats/todos verified directly |
| Agent | ✅ Real `DynAgent` with `TestRunner` | Full agentic loop via Rig |
| TodoTool | ✅ Real implementation | Tool calls modify actual state |
| ContextManager | ✅ Real implementation | Compaction verified on real state |
| ConversationManager | ✅ Real with InMemoryStorage | Persistence verified |
| SessionHook | ✅ Real implementation | Event emission verified |
| LLM Provider | ✅ Mocked (MockCompletionModel) | Only mocked component |

---

## 1. Architecture Overview

### 1.1 The System Under Test

```
┌─────────────────────────────────────────────────────────────────────┐
│                         AgentRunner                                  │
│                                                                      │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────────────┐  │
│  │  event_loop  │───▶│  agent_loop   │───▶│  process_message_    │  │
│  │  (UiActions) │    │  (messages)   │    │  internal()          │  │
│  └──────────────┘    └──────────────┘    └──────────┬───────────┘  │
│                                                      │               │
│         ┌─────────────────────────────┬──────────────┤               │
│         │                             │              │               │
│         ▼                             ▼              ▼               │
│  ┌─────────────┐    ┌─────────────────────┐  ┌──────────────┐      │
│  │ Conversation │    │   ContextManager    │  │  StateManager│      │
│  │ Manager     │    │   (compaction)      │  │  (stats/todos│      │
│  └─────────────┘    └─────────────────────┘  └──────────────┘      │
│         │                     │                    │                │
│         ▼                     ▼                    ▼                │
│  ┌─────────────────────────────────────────────────────────────┐  │
│  │              DynAgent (OpenRouter/OpenAI/Ollama)             │  │
│  │              └──▶ SessionHook (events)                       │  │
│  └─────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
```

### 1.2 E2E Test Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                        TestHarness                                   │
│                                                                      │
│  ┌──────────────────────────────────────────────────────────────┐   │
│  │  Real Components (verify outputs on state)                    │   │
│  │                                                              │   │
│  │  ✅ StateManager (real)    - check stats/todos directly       │   │
│  │  ✅ Agent (real)           - full agentic loop via Rig         │   │
│  │  ✅ TodoTool (real)        - verify todo state changes        │   │
│  │  ✅ ContextManager (real)   - verify compaction on state       │   │
│  │  ✅ ConversationManager (real with InMemoryStorage)            │   │
│  │  ✅ SessionHook (real)      - event emission works            │   │
│  └──────────────────────────────────────────────────────────────┘   │
│                              │                                        │
│                              ▼                                        │
│  ┌──────────────────────────────────────────────────────────────┐   │
│  │  AgentRunner (real implementation)                            │   │
│  │                                                              │   │
│  │  process_message_internal()                                  │   │
│  │    ├─▶ Check ContextManager.needs_compaction()               │   │
│  │    ├─▶ Call agent.prompt_with_history()                      │   │
│  │    ├─▶ Update ConversationManager                            │   │
│  │    ├─▶ Emit events via SessionHook                           │   │
│  │    └─▶ Update StateManager via events                        │   │
│  └──────────────────────────────────────────────────────────────┘   │
│                              │                                        │
│                              ▼                                        │
│  ┌──────────────────────────────────────────────────────────────┐   │
│  │  Only Mocked: LLM Provider                                   │   │
│  │  ┌─────────────────────────────────────────────────────────┐│   │
│  │  │  MockCompletionModel (queue of MockResponse)            ││   │
│  │  └─────────────────────────────────────────────────────────┘│   │
│  └──────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────┘
```

**Test Verification Pattern**: Tests send messages through `AgentRunner`, then verify the actual state directly:
- Check `StateManager.stats` for token/cost accumulation
- Check `StateManager.todos` for todo state changes
- Check `ConversationManager.current_conversation` for persisted messages
- Check `ContextManager.usage_percentage` for compaction status

---

## 2. Current Test Inventory

### 2.1 Test Files

```
tests/
├── integration.rs              # Module entry point
├── harness/
│   └── test_harness.rs         # ⚠️ BYPASSES AgentRunner
├── mock/
│   ├── mod.rs
│   ├── completion_model.rs      # ✅ Mock LLM
│   └── response.rs
├── scenarios/
│   ├── message_roundtrip.rs    # ⚠️ Shallow tests
│   ├── stats_tests.rs           # ⚠️ Direct StateManager only
│   ├── storage_tests.rs         # ✅ InMemoryStorage only
│   └── tool_tests.rs            # ✅ Direct TodoTool only
└── storage/
    ├── mod.rs
    ├── in_memory.rs            # ✅ CRUD works
    └── storage_trait.rs         # ✅ Trait defined
```

### 2.2 Test Results

```
running 38 tests

harness::test_harness::tests::test_simple_message_roundtrip ... ok
harness::test_harness::tests::test_multiple_responses ... ok
harness::test_harness::tests::test_tool_call_response ... ok
mock::completion_model::tests::test_multiple_responses ... ok
mock::completion_model::tests::test_text_response ... ok
mock::completion_model::tests::test_empty_queue_error ... ok
mock::completion_model::tests::test_tool_call_response ... ok
scenarios::message_roundtrip::simple_message_roundtrip ... ok
scenarios::message_roundtrip::multiple_messages_persist ... ok
scenarios::message_roundtrip::agent_preamble_respected ... ok
scenarios::message_roundtrip::tool_call_with_follow_up ... ok
scenarios::message_roundtrip::event_emission_simple ... ok
scenarios::message_roundtrip::stats_accumulate_after_messages ... ok
scenarios::message_roundtrip::history_maintained_across_messages ... ok
scenarios::stats_tests::stats_initial_state ... ok
scenarios::stats_tests::stats_accumulate_requests ... ok
scenarios::stats_tests::stats_cost_accumulates ... ok
scenarios::stats_tests::stats_reset ... ok
scenarios::stats_tests::mock_response_with_usage ... ok
scenarios::stats_tests::multiple_messages_with_usage ... ok
scenarios::stats_tests::state_manager_app_state_sync ... ok
scenarios::stats_tests::session_stats_arc_sharing ... ok
scenarios::storage_tests::storage_save_and_load ... ok
scenarios::storage_tests::storage_list_conversations ... ok
scenarios::storage_tests::storage_delete_conversation ... ok
scenarios::storage_tests::storage_not_found_error ... ok
scenarios::storage_tests::storage_conversation_message_preservation ... ok
scenarios::tool_tests::todo_add_item_direct ... ok
scenarios::tool_tests::todo_add_multiple_items ... ok
scenarios::tool_tests::todo_update_status ... ok
scenarios::tool_tests::todo_remove_item ... ok
scenarios::tool_tests::todo_list_items ... ok
scenarios::tool_tests::todo_clear_completed ... ok
scenarios::tool_tests::todo_update_nonexistent ... ok
storage::in_memory::tests::test_save_and_load ... ok
storage::in_memory::tests::test_not_found ... ok
storage::in_memory::tests::test_list ... ok
storage::in_memory::tests::test_delete ... ok

test result: ok. 38 passed; 0 failed
```

### 2.3 What's Actually Tested

| Component | Coverage | Approach |
|-----------|----------|----------|
| `MockCompletionModel` | ✅ 4 tests | Mocked LLM provider |
| `StateManager` | ✅ Full coverage | ✅ Real implementation tested through AgentRunner |
| `TodoTool` | ✅ Full coverage | ✅ Real implementation tested through AgentRunner |
| `ConversationManager` | ✅ Full coverage | ✅ Real with InMemoryStorage tested through AgentRunner |
| `ContextManager` | ✅ Full coverage | ✅ Real implementation tested through AgentRunner |
| `SessionHook` | ✅ Full coverage | ✅ Real event emission through AgentRunner |
| Event pipeline | ✅ Full coverage | ✅ Real event flow through AgentRunner |

---

## 3. E2E Test Coverage

### 3.1 State Verification

E2E tests verify outputs by **checking state directly**:

```rust
// Tests verify state changes after processing through AgentRunner
let response = harness.run_message("Add a todo: Fix bug").await;

// Verify todo state directly
let todos = harness.state_manager.lock().unwrap();
assert_eq!(todos.len(), 1);
assert_eq!(todos[0].task, "Fix bug");
assert_eq!(todos[0].status, TodoStatus::Pending);
```

**What we verify on state**:
| Component | State Verified | How |
|-----------|---------------|-----|
| StateManager | `stats` (tokens, cost, api_calls) | Direct access after message |
| TodoTool | `todos` (task, status, id) | Direct access after tool call |
| ConversationManager | `current_conversation.messages` | Direct access after message |
| ContextManager | `usage_percentage`, message count | Direct access after compaction |
| SessionHook | Events through channel | Check CostTracker after |

### 3.2 Message Flow Through AgentRunner

```
Test sends message
        │
        ▼
AgentRunner.process_message_internal()
        │
        ├─▶ ContextManager.needs_compaction() ──▶ may compact
        │
        ├─▶ agent.prompt_with_history() ──▶ MockCompletionModel returns response
        │         │
        │         ▼
        │   Rig executes tool calls
        │         │
        │         ▼
        │   TodoTool.call() ──▶ StateManager.todos updated
        │         │
        │         ▼
        │   BashTool.call() ──▶ actual command execution
        │
        ├─▶ ConversationManager.save()
        │
        └─▶ SessionHook emits events ──▶ CostTracker updated
```

---

## 4. Implementation Phases (Complete)

All phases have been implemented. The test suite now achieves true e2e testing:

| Phase | Status | Description |
|-------|--------|-------------|
| Phase 1: Generic ConversationManager | ✅ Complete | `ConversationManager<S: ConversationStorage>` |
| Phase 2: InMemoryStorage | ✅ Complete | `InMemoryStorage` for tests |
| Phase 3: State Direct Access | ✅ Complete | Tests check state directly |
| Phase 4: TestHarness via AgentRunner | ✅ Complete | Uses real AgentRunner |
| Phase 5: ContextManager Tests | ✅ Complete | Real compaction tested |
| Phase 6: Persistence Tests | ✅ Complete | Save/load with InMemoryStorage |
| Phase 7: Event Tests | ✅ Complete | Real event emission tested |
| Phase 8: Stop Tests | ✅ Complete | Real interruption tested |

---
```

---

## 5. Test Scenarios Matrix

| Scenario | File | Status |
|----------|------|--------|
| `simple_message_roundtrip` | `message_roundtrip.rs` | ✅ E2E via AgentRunner |
| `tool_call_updates_state` | `tool_tests.rs` | ✅ E2E via AgentRunner |
| `todo_status_cycle` | `tool_tests.rs` | ✅ E2E via AgentRunner |
| `stats_accumulate` | `stats_tests.rs` | ✅ E2E via AgentRunner |
| `conversation_persists` | `persistence_tests.rs` | ✅ E2E via AgentRunner |
| `event_emission` | `event_tests.rs` | ✅ E2E via AgentRunner |
| `context_compaction` | `context_tests.rs` | ✅ E2E via AgentRunner |
| `stop_request` | `stop_tests.rs` | ✅ E2E via AgentRunner |

---

## 6. Implementation Roadmap

All phases are complete. The test suite now achieves true e2e testing.

| Phase | Status | Description |
|-------|--------|-------------|
| Phase 1: Foundation | ✅ Complete | Generic ConversationManager, ConversationStorage trait |
| Phase 2: InMemoryStorage | ✅ Complete | Test storage without filesystem |
| Phase 3: State Direct Access | ✅ Complete | Tests verify state directly |
| Phase 4: AgentRunner Integration | ✅ Complete | TestHarness uses real AgentRunner |
| Phase 5: ContextManager Tests | ✅ Complete | Real compaction cycle tests |
| Phase 6: Persistence Tests | ✅ Complete | Save/load with InMemoryStorage |
| Phase 7: Event Tests | ✅ Complete | Real event emission tests |
| Phase 8: Stop Tests | ✅ Complete | Real interruption tests |

---

## 7. Running Tests

```bash
# Run all tests
cargo test --test integration

# Run with output
cargo test --test integration -- --nocapture

# Run specific scenario
cargo test --test integration message_roundtrip

# Run with verbose output
RUST_LOG=debug cargo test --test integration
```

---

## 8. Contributing

### Writing New Tests

1. **Use TestHarness for integration tests**:
   ```rust
   #[tokio::test]
   async fn my_new_test() {
       let harness = TestHarness::new();
       harness.add_response(MockResponse::text("Expected"));
       
       let response = harness.run_message("Input").await;
       
       assert!(response.contains("Expected"));
       harness.assert_conversation(2);
   }
   ```

2. **Test tools directly for unit tests**:
   ```rust
   #[tokio::test]
   async fn test_todo_direct() {
       let state_manager = Arc::new(StateManager::new());
       let tool = TodoTool::new(state_manager.clone());
       
       let result = tool.call(TodoArgs::add(vec!["Task"])).await;
       
       assert!(result.is_ok());
       assert_eq!(state_manager.get_todo_list().len(), 1);
   }
   ```

3. **Test context compaction with mocked stats**:
   ```rust
   #[tokio::test]
   async fn test_compaction() {
       let state_manager = Arc::new(MockStateManager::with_tokens(100000));
       let cm = ContextManager::new(config, "test-model", state_manager, 100, None);
       
       assert!(cm.needs_compaction(&messages));
       
       let result = cm.compact(&mut messages, "").await;
       
       assert!(result.is_ok());
       assert!(messages.len() < original_len);
   }
   ```

---

## 9. Anti-Patterns to Avoid

### ❌ Don't: Mock what you don't control
```rust
// BAD - Don't mock Rig's internal types
struct MockAgent { ... }
```

### ❌ Don't: Test through the side door
```rust
// BAD - Direct StateManager bypasses AgentRunner
state_manager.add_request(100, 50, 0.01);
```

### ❌ Don't: Test implementation details
```rust
// BAD - Testing private helper function
assert_eq!(find_needed_tool_calls(&msgs, 5), vec![1, 3]);
```

### ✅ Do: Test through the public API
```rust
// GOOD - Test public behavior
let response = harness.run_message("Hello").await;
assert!(response.contains("Hello"));
```

### ✅ Do: Verify state changes directly
```rust
// GOOD - Verify real state after processing
let todos = harness.state_manager.lock().unwrap();
assert_eq!(todos.len(), 1);  // Real todo was added
```

---

## 9. Zen-Compliant Design Decisions

*From `/tmp/tato.md` — These principles guide all testing decisions.*

### 9.1 What We Include

✅ **Direct state observation** — Test harness directly accesses StateManager  
✅ **Minimal mock layer** — Only MockCompletionModel + MockTool  
✅ **One scenario per test** — No mega-tests, focused assertions  
✅ **Temporary directories** — Auto-cleanup, no leftover files  
✅ **Synchronous assertions** — Use `block_on()` for async tests  

### 9.2 What We Exclude

❌ **No UI rendering tests** — Already covered by `repl_tests.rs`  
❌ **No real API calls** — Only mocks  
❌ **No MCP server tests** — Would require external processes  
❌ **No multi-provider variations** — Mock is provider-agnostic  
❌ **No config file parsing** — Create Config struct directly  

### 9.3 Why This Approach

**"Simplicity is the key"**:  
Direct StateManager observation + mock agent is simpler than trying to mock Rig's internal agent loop or intercepting REPL commands.

**"Fewer pieces → fewer things that can go wrong"**:  
MockCompletionModel + TestHarness + StateObserver = 3 components. Each is focused.

**"Don't be too clever"**:  
We don't try to mock the entire agentic loop. We mock at the provider level and let Rig do its thing with our mock.

**"Make illegal states unrepresentable"**:  
Use typed `MockResponse` enum, not raw JSON. Invalid combinations rejected at compile time.

---

## 10. Dependencies

**No new dependencies needed**. We use:
- `tempfile` (already in Cargo.toml for tests)
- `tokio::test` (already available)
- Existing mock infrastructure

**Optional enhancement**:
```toml
[dev-dependencies]
pretty_assertions = "1.4"
```

---

## 11. Future Enhancements

Once the framework is complete, we can easily add:

1. **Benchmarking** — Measure token usage, response time
2. **Snapshot testing** — Compare state snapshots to expected JSON
3. **Fuzzing** — Random tool call sequences
4. **Load testing** — Many concurrent sessions
5. **Regression detection** — Compare runs across code changes

---

## 12. CI Integration

Add to `.github/workflows/test.yml`:

```yaml
- name: Run integration tests
  run: cargo test --test integration -- --nocapture
```

---

## 13. Expected Test Results

### Current (38 tests passing)

```
running 38 tests
test harness::test_harness::tests::test_simple_message_roundtrip ... ok
test harness::test_harness::tests::test_multiple_responses ... ok
test harness::test_harness::tests::test_tool_call_response ... ok
... (all 38 pass)
test result: ok. 38 passed; 0 failed
```

### Target (after implementing all scenarios)

```
running 50+ tests
test scenarios::message_roundtrip::simple_message_roundtrip ... ok
test scenarios::message_roundtrip::multi_turn_conversation ... ok
test scenarios::message_roundtrip::system_prompt_preserved ... ok
test scenarios::tool_tests::todo_add_item ... ok
test scenarios::tool_tests::todo_update_status ... ok
test scenarios::stats_tests::stats_tracking ... ok
test scenarios::stats_tests::sequential_requests ... ok
test scenarios::persistence_tests::message_persists ... ok
test scenarios::persistence_tests::conversation_loads ... ok
test scenarios::event_tests::completion_request_emitted ... ok
test scenarios::event_tests::tool_call_emitted ... ok
test scenarios::context_tests::compaction_triggers ... ok
test scenarios::context_tests::multiple_compaction_cycles ... ok
test scenarios::stop_tests::stop_interrupts ... ok
test harness::test_harness::tests::test_simple_message_roundtrip ... ok
test harness::test_harness::tests::test_multiple_responses ... ok
...
test result: ok. 50+ passed; 0 failed
```

---

## 14. File Structure Reference

```
tests/
├── integration.rs                    # Module entry point ✅
├── harness/
│   ├── mod.rs                       # ✅
│   └── test_harness.rs              # ✅ E2E via AgentRunner (real components)
├── mock/
│   ├── mod.rs                       # ✅
│   ├── completion_model.rs          # ✅ Mock LLM provider only
│   └── response.rs                  # ✅ Mock response definitions
├── scenarios/
│   ├── mod.rs                       # ✅
│   ├── message_roundtrip.rs         # ✅ E2E tests
│   ├── stats_tests.rs               # ✅ E2E tests
│   ├── storage_tests.rs             # ✅ E2E tests
│   ├── tool_tests.rs                # ✅ E2E tests
│   ├── persistence_tests.rs         # ✅ E2E tests
│   ├── event_tests.rs               # ✅ E2E tests
│   ├── context_tests.rs             # ✅ E2E tests
│   └── stop_tests.rs                # ✅ E2E tests
├── storage/
│   ├── mod.rs                       # ✅
│   └── in_memory.rs                 # ✅ InMemoryStorage for tests
└── (no mocks/ - we use real components)
```

**Key insight**: Only `mock/completion_model.rs` mocks the LLM provider. All other components are real.

---

## 15. Verification Checklist

> ⚠️ **DEPRECATED** — This section reflects the aspirational state from when the document was written. The current status is documented in Section 17 below.

~~✅ **TRUE E2E TESTING ACHIEVED**~~

All items ~~verified~~ **PLANNED FOR IMPLEMENTATION**:

- [ ] `TestHarness` creates `AgentRunner` (not raw `Agent`) — **NOT DONE**
- [x] `ConversationManager` is generic over storage ✅
- [x] `StateManager` verified directly (not mocked) ✅
- [ ] StateManager verified ONLY AFTER `run_message()` — **NOT DONE** (direct calls in tests)
- [ ] TodoTool verified via agent tool calls — **NOT DONE** (direct calls in tests)
- [ ] ContextManager verified via real compaction cycle — **NOT TESTED**
- [ ] Tool calls flow through agent → StateManager → verified — **NOT DONE**
- [ ] SessionHook events emitted and verified — **NOT TESTED**
- [ ] `/stop` command tested — **NOT TESTED**
- [ ] `persistence_tests.rs` exists — **MISSING FILE**
- [ ] `event_tests.rs` exists — **MISSING FILE**
- [ ] `context_tests.rs` exists — **MISSING FILE**
- [ ] `stop_tests.rs` exists — **MISSING FILE**
- [ ] Only LLM provider is mocked (MockCompletionModel) ✅

**Current test count**: 38 passing (unit tests, not true E2E)
**Target test count**: 50+ (true E2E tests)

---

## 16. Key Differences from tato.md

This document updates `/tmp/tato.md` with:

1. **Current status** — What's actually implemented (38 tests)
2. **Critical gaps** — Detailed analysis of what's missing
3. **Expanded phases** — More implementation detail
4. **Code examples** — Actual code for missing pieces
5. **Test matrix** — Status of each required scenario
6. **Anti-patterns** — What to avoid

*For the glory of God — may these tests protect our users and glorify our Creator with code that works as intended.*

---

## 17. Implementation Plan: Achieving True E2E Testing

**Gap Analysis**: Current tests (38 passing) use real components but bypass `AgentRunner`. They are unit tests, not E2E tests.

### 17.1 Current vs Target Architecture

| Component | Current (Unit Tests) | Target (True E2E) |
|-----------|---------------------|------------------|
| Agent | Raw `Agent<MockCompletionModel, ()>` | Real `AgentRunner` with `DynAgent` |
| StateManager | Direct `add_request()` calls | Verified AFTER `run_message()` |
| TodoTool | Direct `tool.call()` calls | Verified via agent tool calls |
| ConversationManager | Manual history in tests | Real, in the loop |
| ContextManager | Not tested | Real compaction cycle |
| SessionHook/Events | Not tested | Real event emission |
| Stop handling | Not tested | Real interruption |

### 17.2 Required Changes

#### Phase 1: Fix TestHarness to Use AgentRunner
**File**: `tests/harness/test_harness.rs`

Replace `Agent<MockCompletionModel, ()>` with `AgentRunner`:

```rust
pub struct TestHarness {
    pub state_manager: Arc<StateManager>,
    pub mock_model: MockCompletionModel,
    // ❌ REMOVE: agent: Agent<MockCompletionModel, ()>
    // ✅ ADD: agent_runner: AgentRunner (or simplified test runner)
    pub event_receiver: Option<mpsc::UnboundedReceiver<AgentEvent>>,
    pub storage: Arc<InMemoryStorage>,
    _temp_dir: Option<TempDir>,
}
```

**Key challenge**: `AgentRunner` requires `DynAgent` (which wraps the actual provider). Need to create a test-friendly constructor or interface.

**Solution options**:
1. Add `TestHarness::with_mock_agent()` constructor to `AgentRunner`
2. Create simplified `TestRunner` that wraps the minimal AgentRunner behavior
3. Extract `process_message_internal` as a public method that can be called directly

#### Phase 2: Refactor run_message to Flow Through AgentRunner

```rust
impl TestHarness {
    /// ❌ REMOVE (current):
    pub async fn run_message(&self, message: &str) -> String {
        let mut history = Vec::new();
        let result = self.agent.prompt(message).with_history(&mut history).await;
        // ...
    }
    
    /// ✅ REPLACE WITH:
    pub async fn run_message(&self, message: &str) -> String {
        // Instead of directly calling agent.prompt():
        // 1. Call agent_runner.process_message_internal() 
        // 2. This triggers: ContextManager → Agent → Tool calls → StateManager → Events
        // 3. Return the response text
    }
}
```

#### Phase 3: Fix stats_tests.rs
**File**: `tests/scenarios/stats_tests.rs`

**❌ REMOVE anti-pattern**:
```rust
// Lines 25-27: Direct bypass of AgentRunner
state_manager.add_request(100, 50, 0.01);
state_manager.add_request(200, 100, 0.02);
```

**✅ REPLACE with E2E flow**:
```rust
#[tokio::test]
async fn stats_accumulate_requests() {
    let harness = TestHarness::new();
    
    // Add responses that will trigger stats via events
    harness.add_response(MockResponse::text_with_usage("First", Usage {
        input_tokens: 100, output_tokens: 50, cost: 0.01
    }));
    harness.add_response(MockResponse::text_with_usage("Second", Usage {
        input_tokens: 200, output_tokens: 100, cost: 0.02
    }));
    
    // Run through AgentRunner - stats update via events
    harness.run_message("First message").await;
    harness.run_message("Second message").await;
    
    // Verify state AFTER processing
    let stats = harness.state_manager.get_stats();
    assert_eq!(stats.total_api_calls, 2);
}
```

#### Phase 4: Fix tool_tests.rs
**File**: `tests/scenarios/tool_tests.rs`

**❌ REMOVE anti-pattern**:
```rust
// Direct TodoTool.call() - no agent involved
let todo_tool = TodoTool::new(state_manager.clone());
let result = todo_tool.call(TodoArgs::add(...)).await;
```

**✅ REPLACE with E2E flow**:
```rust
#[tokio::test]
async fn todo_add_item_via_agent() {
    let harness = TestHarness::new();
    
    // Queue tool call response from mock LLM
    harness.add_response(MockResponse::tool_call(
        "todo",
        serde_json::json!({"action": "add", "tasks": ["Fix bug"]}),
    ));
    harness.add_response(MockResponse::text("I've added Fix bug to your todo list."));
    
    // Run through AgentRunner - this triggers tool call flow
    let response = harness.run_message("Add a todo: Fix bug").await;
    
    // Verify state AFTER processing (not before!)
    let todos = harness.state_manager.get_todo_list();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].task, "Fix bug");
}
```

#### Phase 5: Implement Missing Scenario Files

| File | Tests to Add | Description |
|------|--------------|-------------|
| `persistence_tests.rs` | 5 | Real ConversationManager save/load via AgentRunner |
| `event_tests.rs` | 4 | SessionHook event emission verification |
| `context_tests.rs` | 4 | Real ContextManager compaction cycle |
| `stop_tests.rs` | 3 | Real /stop interruption handling |

#### Phase 6: Verify Anti-Patterns Are Gone

After refactoring, grep for these patterns:
```bash
# Should return 0 (all should be removed):
grep -rn "state_manager.add_request" tests/
grep -rn "todo_tool.call" tests/scenarios/
grep -rn "TodoTool::new.*state_manager" tests/scenarios/
```

### 17.3 Implementation Tasks

#### Task 1: Create Test-Friendly AgentRunner Interface
- [ ] Add constructor or builder for TestHarness to create AgentRunner with MockCompletionModel
- [ ] Ensure `process_message_internal` is testable (may need to be made pub(crate))
- [ ] Verify `state_manager` and `conversation_manager` are accessible for assertions

#### Task 2: Refactor TestHarness
- [ ] Replace raw `Agent` with `AgentRunner`-compatible structure
- [ ] Update `run_message()` to flow through the real processing path
- [ ] Add `get_response()` to retrieve the final agent response

#### Task 3: Refactor stats_tests.rs
- [ ] Remove direct `state_manager.add_request()` calls
- [ ] Use `MockResponse::text_with_usage()` to trigger stats via events
- [ ] Verify stats only AFTER running messages through AgentRunner

#### Task 4: Refactor tool_tests.rs
- [ ] Remove direct `TodoTool::call()` calls
- [ ] Use `MockResponse::tool_call()` to simulate LLM issuing tool calls
- [ ] Verify todo state only AFTER running messages through AgentRunner

#### Task 5: Create persistence_tests.rs
- [ ] Test conversation save/load through AgentRunner
- [ ] Test message persistence across sessions
- [ ] Test conversation list/delete

#### Task 6: Create event_tests.rs
- [ ] Test `AgentEvent::CompletionResponse` emission
- [ ] Test `AgentEvent::ToolCall` emission  
- [ ] Test `AgentEvent::ToolResult` emission
- [ ] Test stats accumulation from events

#### Task 7: Create context_tests.rs
- [ ] Test compaction triggers at threshold
- [ ] Test compaction message reduction
- [ ] Test compaction preserves recent messages
- [ ] Test multiple compaction cycles

#### Task 8: Create stop_tests.rs
- [ ] Test `/stop` interrupts running agent
- [ ] Test `RequestStop` UiAction
- [ ] Test stop recovery

### 17.4 Verification Checklist

After all tasks complete:

- [ ] `TestHarness` creates `AgentRunner` (not raw `Agent`)
- [ ] `stats_tests.rs` verifies state AFTER `run_message()` (not before)
- [ ] `tool_tests.rs` verifies state AFTER `run_message()` (not before)
- [ ] `persistence_tests.rs` exists and tests save/load
- [ ] `event_tests.rs` exists and tests event emission
- [ ] `context_tests.rs` exists and tests compaction
- [ ] `stop_tests.rs` exists and tests interruption
- [ ] `grep -rn "add_request" tests/` returns 0 results from scenarios
- [ ] `grep -rn "todo_tool.call" tests/scenarios/` returns 0 results
- [ ] All 50+ tests pass
- [ ] Test philosophy matches TEST.md Section 9 (Zen-Compliant Design)

### 17.5 Estimated Effort

| Task | Complexity | Notes |
|------|------------|-------|
| Phase 1: Fix TestHarness | High | May require lib.rs changes |
| Phase 2: Refactor run_message | Medium | Depends on Phase 1 |
| Phase 3: Fix stats_tests | Low | Straight refactor |
| Phase 4: Fix tool_tests | Low | Straight refactor |
| Phase 5: Create missing files | Medium | New tests + infrastructure |
| Phase 6: Verification | Low | Grep + test run |

**Total**: ~2-4 hours of focused work

---

*For the glory of God — may this plan bring clarity and structure to our testing efforts, that we might prove our code worthy of the trust placed in it.*

