# Task 7: Context Compaction - Implementation Plan

## Overview
Implement automatic context management to handle conversations that grow too large. When the conversation context (chat history) approaches the model's context window limit, the system should compact or summarize older messages to make room for new ones.

## User Story
As a user, I want to have long conversations without hitting context window limits, so that:
1. I can work on complex tasks that require many back-and-forth exchanges
2. The agent maintains context across extended sessions
3. I don't lose access to important parts of the conversation history

---

## Implementation Plan

### Phase 1: Core Infrastructure

#### Step 1.1: Add Configuration Support
- Update `src/config.rs` to add:
  ```rust
  #[derive(Debug, Deserialize, Clone, Default)]
  pub struct ContextConfig {
      /// Compaction threshold (0.0-1.0), default 0.8
      #[serde(default = "default_threshold")]
      pub threshold: f64,
      /// Keep last N messages always
      #[serde(default = "default_keep_recent")]
      pub keep_recent: usize,
      /// Enable/disable compaction
      #[serde(default = "default_enabled")]
      pub enabled: bool,
      /// Model context window size (0 = auto-detect from API)
      #[serde(default)]
      pub context_window: Option<usize>,
  }
  
  fn default_threshold() -> f64 { 0.8 }
  fn default_keep_recent() -> usize { 5 }
  fn default_enabled() -> bool { true }
  ```
- Add to Config struct: `pub context: Option<ContextConfig>`
- Add environment variables: `CONTEXT_THRESHOLD`, `CONTEXT_ENABLED`, etc.

#### Step 1.2: Create Token Estimator
- Create `src/token_estimator.rs`:
  ```rust
  use rig::completion::message::Message;
  
  /// Trait for estimating token counts
  pub trait TokenEstimator {
      fn estimate(&self, text: &str) -> usize;
      fn estimate_message(&self, msg: &Message) -> usize;
      fn estimate_messages(&self, msgs: &[Message]) -> usize;
  }
  
  /// Simple char-based estimator (4 chars ≈ 1 token)
  pub struct SimpleEstimator;
  
  /// Tiktoken-based estimator (more accurate)
  pub struct TiktokenEstimator { ... }
  ```
- Implement both SimpleEstimator (fallback) and TiktokenEstimator (if available)
- Use OpenRouter API to get model context_window if not configured

#### Step 1.3: Create Context Manager
- Create `src/context_manager.rs`:
  ```rust
  pub struct ContextManager<M: CompletionModel, P: PromptHook<M>> {
      config: ContextConfig,
      estimator: Box<dyn TokenEstimator>,
      context_window: usize,
      current_token_count: usize,
      /// Reference to agent for making summarization calls
      agent: Option<Arc<Agent<M, P>>>,
  }
  
  impl<M: CompletionModel, P: PromptHook<M>> ContextManager<M, P> {
      pub fn new(config: ContextConfig, context_window: usize) -> Self;
      pub fn with_agent(config: ContextConfig, context_window: usize, agent: Arc<Agent<M, P>>) -> Self;
      pub fn needs_compaction(&self, messages: &[Message]) -> bool;
      
      /// Hybrid approach: summarize old messages, keep recent ones
      pub async fn compact(&mut self, messages: &mut Vec<Message>) -> Result<CompactionResult>;
      
      /// Internal: summarize a range of messages using the model
      async fn summarize_messages(&self, messages: &[Message]) -> Result<String>;
      
      pub fn estimate_total_tokens(&self, messages: &[Message], system_prompt: &str) -> usize;
  }
  
  #[derive(Debug, Clone)]
  pub struct CompactionResult {
      pub original_count: usize,
      pub compacted_count: usize,
      pub tokens_saved: usize,
      pub num_summarized: usize,
  }
  ```

### Phase 2: Compaction Logic

#### Step 2.1: Hybrid Truncation/Summarization Approach
The strategy is to summarize older messages and keep recent ones:
- **Step A**: Identify messages to summarize (everything except last N messages)
- **Step B**: Call model to generate a summary of those older messages
- **Step C**: Replace the old messages with a single "Summary" message
- **Step D**: Keep last N messages (keep_recent config) as-is for immediate context
- **Step E**: If total still exceeds threshold, truncate from the summary

Example flow:
```
Before (100 messages):
[Msg 1] → [Msg 2] → ... → [Msg 95] → [Msg 96] → [Msg 97] → [Msg 98] → [Msg 99] → [Msg 100]

keep_recent = 5

Step A: Identify messages to summarize (1-95)
Step B: Summarize messages 1-95 into a single summary
Step C: Replace with [Summary of 1-95]

After (6 messages):
[Summary of msgs 1-95] → [Msg 96] → [Msg 97] → [Msg 98] → [Msg 99] → [Msg 100]
```

#### Step 2.2: Implement Summarization Logic
- Create a summarization prompt:
  ```
  Summarize the following conversation concisely, preserving key information:
  - What the user wanted to accomplish
  - Important decisions made
  - Key outputs or results
  - Any pending tasks or follow-ups
  
  Conversation to summarize:
  {messages}
  ```
- Call the model with this prompt
- Replace the summarized messages with a single message:
  ```rust
  Message::Assistant {
      content: format!("[Summary of {} messages]\n\n{}", 
          num_summarized, 
          summary),
      timestamp: Utc::now(),
  }
  ```

#### Step 2.3: Implement Final Truncation (if needed)
- After summarization, check if still over threshold
- If yes, truncate from the summary message (it can be split)
- Always keep last N messages intact

#### Step 2.4: Handle Edge Cases
- Empty history: No compaction needed
- Single message: No compaction needed
- All messages are "keep_recent": Skip summarization, no truncation needed
- Summarization fails: Fall back to pure truncation
- Message too large alone: Truncate the message itself

### Phase 3: Integration

#### Step 3.1: Update AgentRunner
- Add ContextManager to AgentRunner struct:
  ```rust
  pub struct AgentRunner<M: CompletionModel, P: PromptHook<M>> {
      agent: Agent<M, P>,
      config: Config,
      skills: SkillRegistry,
      stats: Arc<Mutex<SessionStats>>,
      context_manager: Option<ContextManager>,  // NEW
  }
  ```
- In the run() loop, before agent.prompt():
  ```rust
  // Check and compact if needed
  if let Some(ref mut cm) = self.context_manager {
      if cm.needs_compaction(&chat_history) {
          let result = cm.compact(&mut chat_history);
          println!("[Context compacted: {} → {} messages]", 
              result.original_count, result.compacted_count);
      }
  }
  ```

#### Step 3.2: Update Token Tracking
- After each successful response, update token count in context manager:
  ```rust
  if let Some(ref mut cm) = self.context_manager {
      cm.update_token_count(response.usage.input_tokens);
  }
  ```

#### Step 3.3: Add REPL Commands
- `/compact` - Force compaction now
- `/context` - Show context usage:
  ```
  Context: 45,000 / 200,000 tokens (22.5%)
  150 messages
  Compaction threshold: 80%
  ```

### Phase 4: Testing

#### Step 4.1: Unit Tests
- Test token estimation accuracy (compare to tiktoken)
- Test truncation preserves recent messages
- Test needs_compaction returns correct result

#### Step 4.2: Integration Tests
- Test full compaction flow in REPL
- Test edge cases (empty, very long messages)
- Test with various model context sizes

#### Step 4.3: Manual Testing
- Test with long conversation
- Verify context compaction happens correctly
- Verify key information preserved

---

## File Changes Summary

| File | Changes |
|------|---------|
| `src/config.rs` | Add ContextConfig struct, add to Config, add env var parsing |
| `src/token_estimator.rs` | NEW - TokenEstimator trait and implementations |
| `src/context_manager.rs` | NEW - ContextManager, CompactionResult |
| `src/lib.rs` | Add context_manager field to AgentRunner, integrate into run() loop |

---

## Dependencies on Other Tasks
- **Task 3 (Token Counting)**: Uses token tracking for the actual API usage, but we need our own estimator for pre-request estimation

---

## Open Questions
1. Should we include tool definitions in token count? (They're passed separately in rig)
2. How to handle different model context windows?
3. Should we cache token counts instead of recalculating each time?

---

## Estimated Complexity
- **Time**: ~2-3 days
- **Risk**: Medium - impacts core REPL loop
- **Testing**: Requires manual testing with long conversations

---

## Checkbox Summary

### Phase 1: Core Infrastructure
- [x] **1.1**: Add ContextConfig to src/config.rs
- [x] **1.2**: Create token_estimator.rs with TokenEstimator trait
- [x] **1.3**: Create context_manager.rs with ContextManager struct

### Phase 2: Compaction Logic
- [x] **2.1**: Implement hybrid truncation/summarization approach
- [x] **2.2**: Implement summarization logic (call model to summarize)
- [x] **2.3**: Implement final truncation (if summary still too large)
- [x] **2.4**: Handle edge cases

### Phase 3: Integration
- [x] **3.1**: Update AgentRunner with context_manager field
- [x] **3.2**: Update token tracking from API responses
- [x] **3.3**: Add /compact and /context REPL commands

### Phase 4: Testing
- [x] **4.1**: Unit tests for token estimation
- [x] **4.2**: Integration tests
- [ ] **4.3**: Manual testing