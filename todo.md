# PeakBot Development Tasks

This document outlines the planned features and improvements for PeakBot. Each task is expanded into ticket-style subtasks for implementation by an agent.

---

## Task 1: Dynamic MCP Server Enabling/Disabling

### Overview
Implement the ability to have MCP servers in a disabled state that can be dynamically enabled during runtime via a special internal tool. When disabled, the model receives a prompt informing it about the available but disabled MCP tools, allowing it to enable them when needed.

### User Story
As a user, I want to optionally configure MCP servers as disabled (`enabled: false`) in my config so that:
1. They don't consume resources until needed
2. The model is aware of their existence and can enable them when appropriate
3. I have fine-grained control over which MCP tools are available

### Implementation Details

#### 1.1 Update Config Structure
- [ ] Add `enabled` field to `McpServerConfig` in `src/config.rs`
- [ ] Set default value to `true` (backward compatible)
- [ ] Update YAML parsing to support the new field
- [ ] Add environment variable support (`MCP_SERVERS` JSON should include enabled state)

#### 1.2 Create MCP Server Registry
- [ ] Create new module `src/mcp_registry.rs`
- [ ] Implement `McpServerRegistry` struct that tracks:
  - All configured MCP servers (enabled and disabled)
  - Currently active/enabled MCP server handles
  - Server metadata (name, tools available, enabled state)
- [ ] Implement methods:
  - `new()` - create empty registry
  - `register_disabled_server(config)` - add server without connecting
  - `enable_server(name)` - connect to a previously disabled server
  - `disable_server(name)` - disconnect and remove tools (optional)
  - `get_available_servers()` - list disabled servers with their tool descriptions

#### 1.3 Create "Enable MCP" Internal Tool
- [ ] Create new tool `src/tools/enable_mcp.rs` implementing `rig::tool::Tool`
- [ ] Tool name: `enable_mcp_server`
- [ ] Input schema:
  ```json
  {
    "type": "object",
    "properties": {
      "server_name": {
        "type": "string",
        "description": "The name of the MCP server to enable"
      }
    },
    "required": ["server_name"]
  }
  ```
- [ ] Behavior:
  1. Look up server in registry
  2. If already enabled, return "Server '{name}' is already enabled"
  3. If disabled, connect to MCP server
  4. Add tools to agent's tool set
  5. Return "Successfully enabled MCP server '{name}' with {n} tools"

#### 1.4 Update Agent Builder
- [ ] Modify `build_agent()` in `src/lib.rs` to:
  1. Accept `McpServerRegistry` instead of `&[McpServerHandle]`
  2. Only connect to enabled servers at startup
  3. Pass registry to enable_mcp tool
- [ ] Add `enable_mcp` tool to the agent

#### 1.5 System Prompt Updates
- [ ] Create section in system prompt describing:
  1. Available but disabled MCP servers
  2. How to enable them using `enable_mcp_server`
  3. What each disabled server provides (tool list from metadata)

#### 1.6 Update REPL for Dynamic Tool Addition
- [ ] Modify `AgentRunner` to handle tool additions at runtime
- [ ] When `enable_mcp` succeeds, add new tools to the agent dynamically
- [ ] This may require storing agent as `Arc<RwLock<Agent>>` or similar

#### 1.7 Testing
- [ ] Write unit tests for `McpServerRegistry`
- [ ] Test enable flow with mock MCP server
- [ ] Test error cases (server not found, already enabled, connection failure)

---

## Task 2: Implement the "think" Tool

### Overview
Add Anthropic's "think" tool to give Claude dedicated space for structured thinking during complex tool use situations. This is different from extended thinking - it's for reasoning after receiving tool outputs.

### User Story
As a model, I want a "think" tool that allows me to:
- Process and analyze complex tool outputs before acting
- Brainstorm multiple approaches to solve a problem
- Check policy compliance and verify all information is collected
- Handle sequential decision making where each step builds on previous ones

### Implementation Details

#### 2.1 Create Think Tool
- [x] Create new tool `src/tools/think.rs` implementing `rig::tool::Tool`
- [x] Tool name: `think`
- [x] Input schema:
  ```json
  {
    "type": "object",
    "properties": {
      "thought": {
        "type": "string",
        "description": "Your thoughts. Use it when complex reasoning or brainstorming is needed. For example, if you explore the repo and discover the source of a bug, call this tool to brainstorm several unique ways of fixing the bug, and assess which change(s) are likely to be simplest and most effective."
      }
    },
    "required": ["thought"]
  }
  ```
- [x] Behavior: Simply echo back the thought with a prefix like "Thinking: {thought}"

#### 2.2 Add Think Tool to Agent
- [x] Import and add `ThinkTool` to agent in `build_agent()`
- [x] Ensure it's available alongside other built-in tools

#### 2.3 Update System Prompt
- [x] Add section explaining when to use the think tool:
  ```
  ## Using the think tool

  Before taking any action or responding to the user after receiving tool results, 
  use the think tool as a scratchpad to:
  - Analyze the tool output and extract relevant information
  - Check if all required information has been collected
  - Brainstorm multiple approaches to solve the problem
  - Verify the planned action is correct and safe
  - Iterate over tool results for correctness
  
  Use think when:
  - You need to carefully process complex tool outputs
  - You're dealing with multi-step problems with sequential decisions
  - You need to follow detailed guidelines or policies
  - Mistakes are costly and you want to verify your approach
  ```

#### 2.5 Testing
- [x] Test tool is callable and returns thought
- [x] Verify it appears in tool definitions sent to model

---

## Task 3: Token Counting and Cost Tracking via PromptHook

### Overview
Implement token usage and cost tracking using rig-core's `PromptHook` trait. Track input/output tokens for each request and accumulate costs based on model pricing.

### User Story
As a user, I want to see token usage and API costs for my sessions so that I can:
1. Monitor spending
2. Understand the cost of different operations
3. Optimize prompts and tool usage

### Implementation Details

#### 3.1 Create Token/Cost Tracking Hook
- [x] Create new module `src/hooks/mod.rs`
- [x] Create `TokenCostHook` struct implementing `PromptHook<M>`
- [x] Define model pricing (can be expanded later):
  ```rust
  pub struct ModelPricing {
      pub input_per_million: f64,
      pub output_per_million: f64,
  }
  
  // Default prices (example for Claude 3.7 Sonnet)
  pub fn get_pricing(model: &str) -> ModelPricing {
      match model {
          "anthropic/claude-3.7-sonnet" => ModelPricing {
              input_per_million: 3.0,   // $3.00 per million input tokens
              output_per_million: 15.0, // $15.00 per million output tokens
          },
          // Add more models as needed
          _ => ModelPricing {
              input_per_million: 3.0,
              output_per_million: 15.0,
          },
      }
  }
  ```

#### 3.2 Implement PromptHook Methods
- [x] Implement `on_completion_call` to log/track request start
- [x] Implement `on_completion_response` to:
  - Extract token usage from response
  - Calculate costs
  - Log/display metrics
- [x] Implement `on_tool_call` and `on_tool_result` for detailed tool logging

#### 3.3 Create Statistics Struct
- [x] Create `SessionStats` to track:
  - Total input tokens
  - Total output tokens
  - Total API calls
  - Total cost (accumulated)
  - Per-request history
- [ ] Implement methods:
  - `new()` - create empty stats
  - `add_request(input_tokens, output_tokens, cost)` - add a request
  - `summary()` - get formatted summary string
  - `reset()` - clear stats

#### 3.4 Display Stats in REPL
- [x] After each model response, display:
  ```
  [Tokens: {input} in / {output} out | Cost: ${cost} | Total: ${total}]
  ```
- [x] Add command to show cumulative stats (e.g., `/stats`)

#### 3.5 Make Hook Pluggable
- [x] Design `TokenCostHook` to be optional
- [x] Add config option to enable/disable cost tracking
- [x] Allow custom pricing via config file

#### 3.6 Testing
- [x] Test hook is called on each request
- [x] Test token calculation accuracy
- [x] Test cost calculation with known prices

---

## Task 4: Refactor Tool Logging with Prompt Hooks

### Overview
Refactor existing tool logging (currently in `LoggingToolDyn`) to use PromptHooks for a more centralized, consistent approach. Move from wrapper-based to hook-based logging.

### User Story
As a developer, I want tool execution to be logged consistently through a central hook system so that:
1. Logging behavior is configurable in one place
2. We can easily add filters, metrics, or exports
3. MCP and built-in tools are logged uniformly

### Implementation Details

#### 4.1 Create Comprehensive Logging Hook
- [ ] Create `ToolLoggingHook` in `src/hooks/logging.rs`
- [ ] Implement all relevant PromptHook methods:
  - `on_tool_call` - log before execution
  - `on_tool_result` - log after execution with duration and result status
  - `on_completion_call` - log request start
  - `on_completion_response` - log request completion

#### 4.2 Define Logging Format
- [ ] Create structured log format with fields:
  - `timestamp` - ISO 8601
  - `tool_name` - name of the tool
  - `server` - MCP server name (for MCP tools), "builtin" for built-in
  - `duration_ms` - execution time
  - `args` - tool arguments (truncated if too long)
  - `result_status` - "success" or "error"
  - `result_len` - length of result
  - `error` - error message if failed

#### 4.3 Migrate from LoggingToolDyn
- [ ] Keep `LoggingToolDyn` for backward compatibility
- [ ] Make it optionally use the new hook system
- [ ] Gradually migrate logging statements to use hooks
- [ ] Document when to use wrapper vs hook

#### 4.4 Add Filtering and Configuration
- [ ] Add config options:
  - `log_tools: bool` - enable/disable tool logging
  - `log_tool_args: bool` - include args in logs
  - `log_tool_results: bool` - include results in logs
  - `log_level: "debug" | "info" | "warn"` - verbosity
- [ ] Implement filtering in hook based on config

#### 4.5 Unified MCP Logging
- [ ] Remove per-MCP-server logging in `McpServerHandle::dyn_tools()`
- [ ] Use hook's `server` field to identify MCP source
- [ ] This provides consistent logging across all MCP servers

#### 4.6 Remove Redundant Logging
- [ ] Audit existing `tracing::info!` calls in tool implementations
- [ ] Remove duplicate logging that now happens via hooks
- [ ] Keep important operational logs (connection status, etc.)

#### 4.7 Testing
- [ ] Verify all tools log through hooks
- [ ] Test log format consistency
- [ ] Test configuration options work correctly
- [ ] Verify MCP tools include server name

---

## Task 5: Support Local Ollama/llama.cpp Server

### Overview
Add support for connecting to a local Ollama server (or llama.cpp server) as an alternative to OpenRouter. This enables offline usage and local model inference.

### User Story
As a user, I want to use a local LLM via Ollama so that:
1. I can work offline
2. I have privacy (data stays local)
3. I can use custom or fine-tuned models
4. I can avoid API costs for development/testing

### Implementation Details

#### 5.1 Update Config for Ollama
- [ ] Add new config section in `Config`:
  ```rust
  #[serde(default)]
  pub ollama: Option<OllamaConfig>,
  
  #[derive(Debug, Deserialize, Clone, Default)]
  pub struct OllamaConfig {
      /// Base URL (default: http://localhost:11434)
      #[serde(default = "default_ollama_url")]
      pub base_url: String,
      /// Model name (e.g., "llama3", "mistral", "codellama")
      pub model: String,
      /// Temperature setting (optional)
      #[serde(default)]
      pub temperature: Option<f32>,
      /// Number of context tokens (optional, default from Ollama)
      #[serde(default)]
      pub num_ctx: Option<usize>,
  }
  
  fn default_ollama_url() -> String {
      "http://localhost:11434".to_string()
  }
  ```
- [ ] Add environment variables:
  - `OLLAMA_BASE_URL`
  - `OLLAMA_MODEL`
  - `OLLAMA_TEMPERATURE`
  - `OLLAMA_NUM_CTX`

#### 5.2 Create Ollama Client/Provider
- [ ] Check if rig-core has Ollama support (likely via generic HTTP client)
- [ ] If not built-in, create custom implementation:
  - [ ] Create `src/providers/ollama.rs`
  - [ ] Implement `CompletionModel` trait for Ollama
  - [ ] Use reqwest for HTTP requests to Ollama's `/api/completions` endpoint
  - [ ] Handle both chat and completion endpoints

#### 5.3 Detect and Use Appropriate Provider
- [ ] Modify `build_agent()` to detect provider:
  - If Ollama config present -> use Ollama client
  - Otherwise -> use OpenRouter client (current behavior)
- [ ] Create factory function:
  ```rust
  pub fn create_provider(config: &Config) -> Result<ProviderType> {
      if let Some(ollama) = &config.ollama {
          // Create Ollama provider
      } else if config.openrouter_api_key.is_some() {
          // Create OpenRouter provider  
      } else {
          anyhow::bail!("No provider configured")
      }
  }
  ```

#### 5.4 Handle Tool Definitions Format Difference
- [ ] OpenRouter uses OpenAI-compatible tool format
- [ ] Ollama may have different format requirements
- [ ] Create adapter to convert tool definitions to Ollama format
- [ ] Test with various Ollama models

#### 5.5 Update REPL for Multiple Providers
- [ ] Update startup message to show which provider is active
- [ ] Handle provider-specific error messages
- [ ] Add connection check for Ollama at startup

#### 5.6 Document Usage
- [ ] Update `agents.md` with Ollama configuration examples
- [ ] Document required Ollama model capabilities (function calling)
- [ ] Add troubleshooting section for common Ollama issues

#### 5.7 Testing
- [ ] Test connection to local Ollama instance
- [ ] Test tool calling works (if model supports it)
- [ ] Test error handling when Ollama is not running
- [ ] Test with different models ( Llama3, Mistral, etc.)

---

## Task 6: Debug ApiResponse JsonError

### Overview
Investigate and fix an intermittent error that occurs during completions:
```
Error: CompletionError: JsonError: data did not match any variant of untagged enum ApiResponse
```

This error suggests there's a mismatch between the expected response format from OpenRouter and what the rig-core library expects.

### User Story
As a user, I want completions to work reliably without intermittent JSON parsing errors so that the agent doesn't crash or fail unexpectedly during normal operation.

### Implementation Details

#### 6.1 Gather More Information
- [ ] Search for existing issues in rig-core repo related to this error
- [ ] Look at OpenRouter API response formats
- [ ] Check if this happens with specific models or all models
- [ ] Identify when it occurs (first request, streaming, tool calls, etc.)

#### 6.2 Analyze the Error Source
- [ ] Examine rig-core's `ApiResponse` enum definition
- [ ] Check what response variants are expected
- [ ] Compare with actual OpenRouter response format
- [ ] Identify which variant is failing to parse

#### 6.3 Reproduce and Isolate
- [ ] Try to reproduce the error consistently
- [ ] Add debug logging to capture the raw response when error occurs
- [ ] Check if it's related to:
  - Streaming vs non-streaming responses
  - Tool use vs text-only responses
  - Specific response fields (usage, stop_reason, etc.)
  - Error responses from the API

#### 6.4 Fix the Issue
Potential solutions:
- [ ] Update rig-core to a version with the fix
- [ ] Add request configuration to avoid triggering the issue
- [ ] Patch or work around the issue in PeakBot
- [ ] Add retry logic with exponential backoff for this specific error

#### 6.5 Add Error Handling
- [ ] Add specific handling for this error type
- [ ] Implement retry mechanism for transient failures
- [ ] Provide clearer error messages to users
- [ ] Add telemetry/metrics for tracking frequency

#### 6.6 Testing
- [ ] Write test that reproduces the issue (if reproducible)
- [ ] Verify fix resolves the issue
- [ ] Add monitoring for regression

---

## Task 7: Context Compaction

### Overview
Implement automatic context management to handle conversations that grow too large. When the conversation context (chat history) approaches the model's context window limit, the system should compact older messages while preserving recent context.

### User Story
As a user, I want to have long conversations without hitting context window limits, so that:
1. I can work on complex tasks that require many back-and-forth exchanges
2. The agent maintains context across extended sessions
3. I don't lose access to important parts of the conversation history

### Hybrid Approach: Summarize + Keep Recent
The compaction uses a hybrid strategy:
1. **Summarize older messages**: Everything except the last N messages is summarized by the model
2. **Keep recent messages**: Last N messages (configurable) are kept verbatim for immediate context
3. **Final truncation if needed**: If the summary is still too large, truncate from the summary

Example:
```
Before (100 messages, keep_recent=5):
[Msg 1] → ... → [Msg 95] → [Msg 96] → [Msg 97] → [Msg 98] → [Msg 99] → [Msg 100]

After compaction:
[Summary of msgs 1-95] → [Msg 96] → [Msg 97] → [Msg 98] → [Msg 99] → [Msg 100]
```

### Implementation Plan

#### Phase 1: Core Infrastructure

**Step 1.1: Add Configuration Support**
- Update `src/config.rs` to add:
  ```rust
  #[derive(Debug, Deserialize, Clone, Default)]
  pub struct ContextConfig {
      /// Compaction threshold (0.0-1.0), default 0.8
      #[serde(default = "default_threshold")]
      pub threshold: f64,
      /// Compaction strategy: "truncate" or "summarize"
      #[serde(default = "default_strategy")]
      pub strategy: String,
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
  fn default_strategy() -> String { "truncate".to_string() }
  fn default_keep_recent() -> usize { 5 }
  fn default_enabled() -> bool { true }
  ```
- Add to Config struct: `pub context: Option<ContextConfig>`
- Add environment variables: `CONTEXT_THRESHOLD`, `CONTEXT_STRATEGY`, `CONTEXT_ENABLED`, etc.

**Step 1.2: Create Token Estimator**
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

**Step 1.3: Create Context Manager**
- Create `src/context_manager.rs`:
  ```rust
  pub struct ContextManager {
      config: ContextConfig,
      estimator: Box<dyn TokenEstimator>,
      context_window: usize,
      current_token_count: usize,
  }
  
  impl ContextManager {
      pub fn new(config: ContextConfig, context_window: usize) -> Self;
      pub fn needs_compaction(&self, messages: &[Message]) -> bool;
      pub fn compact(&mut self, messages: &mut [Message]) -> CompactionResult;
      pub fn estimate_total_tokens(&self, messages: &[Message], system_prompt: &str) -> usize;
  }
  
  #[derive(Debug, Clone)]
  pub struct CompactionResult {
      pub original_count: usize,
      pub compacted_count: usize,
      pub tokens_saved: usize,
      pub strategy: String,
  }
  ```

#### Phase 2: Compaction Logic

**Step 2.1: Implement Truncation Strategy**
- Calculate tokens needed: context_window * threshold
- Separate messages into:
  - **Preserve always**: System prompt, tool definitions (passed separately)
  - **Never compact**: Last N messages (keep_recent config)
  - **Compaction candidates**: Everything in between
- Remove oldest messages from candidates until under threshold
- Return CompactionResult with details

**Step 2.2: Implement Summarization Strategy (Future)**
- Identify messages to summarize (older than X turns)
- Create a summarization prompt
- Call model to generate summary
- Replace original messages with summary + key metadata
- Note: This requires an extra API call, so make it optional

**Step 2.3: Handle Edge Cases**
- Empty history: No compaction needed
- Single message: Check if it's too large, truncate if needed
- All messages are "keep_recent": No compaction
- Message too large alone: Truncate the message itself

#### Phase 3: Integration

**Step 3.1: Update AgentRunner**
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

**Step 3.2: Update Token Tracking**
- After each successful response, update token count in context manager:
  ```rust
  if let Some(ref mut cm) = self.context_manager {
      cm.update_token_count(response.usage.input_tokens);
  }
  ```

**Step 3.3: Add REPL Commands**
- `/compact` - Force compaction now
- `/context` - Show context usage:
  ```
  Context: 45,000 / 200,000 tokens (22.5%)
  150 messages
  Compaction threshold: 80%
  Strategy: truncate
  ```

#### Phase 4: Testing

**Step 4.1: Unit Tests**
- Test token estimation accuracy (compare to tiktoken)
- Test truncation preserves recent messages
- Test needs_compaction returns correct result

**Step 4.2: Integration Tests**
- Test full compaction flow in REPL
- Test edge cases (empty, very long messages)
- Test with various model context sizes

**Step 4.3: Manual Testing**
- Test with long conversation
- Verify context compaction happens correctly
- Verify key information preserved

### File Changes Summary

| File | Changes |
|------|---------|
| `src/config.rs` | Add ContextConfig struct, add to Config, add env var parsing |
| `src/token_estimator.rs` | NEW - TokenEstimator trait and implementations |
| `src/context_manager.rs` | NEW - ContextManager, CompactionResult |
| `src/lib.rs` | Add context_manager field to AgentRunner, integrate into run() loop |

### Dependencies on Other Tasks
- **Task 3 (Token Counting)**: Uses token tracking for the actual API usage, but we need our own estimator for pre-request estimation

### Open Questions
1. Should we include tool definitions in token count? (They're passed separately in rig)
2. How to handle different model context windows?
3. Should we cache token counts instead of recalculating each time?

### Estimated Complexity
- **Time**: ~3-4 days (requires model API call for summarization)
- **Risk**: Medium - impacts core REPL loop
- **Testing**: Requires manual testing with long conversations

--- 

## Task 8: Domain-Specific Thinking Prompts

### Overview
Add domain-specific thinking examples to the system prompt to help the model use the think tool more effectively in complex domains. This follows Anthropic's recommendation for complex domains where structured thinking examples can significantly improve reasoning quality.

### User Story
As a model, I want domain-specific guidance and examples for using the think tool so that I can:
- Apply appropriate reasoning frameworks for different task types
- Know what to look for when analyzing domain-specific problems
- Follow best practices for complex decision-making in specialized areas

### Implementation Details

#### 8.1 Analyze Common Task Domains
- [ ] Identify key domains where PeakBot operates (e.g., code review, bug fixing, refactoring, documentation)
- [ ] For each domain, determine the typical reasoning patterns needed

#### 8.2 Create Domain-Specific Thinking Guides
- [ ] Add thinking templates/examples for code review:
  - What to look for in security, performance, readability
  - How to prioritize issues found
  - When to suggest vs. mandate changes
- [ ] Add thinking templates/examples for bug fixing:
  - Reproducing the issue
  - Identifying root cause vs. symptoms
  - Evaluating fix approaches
- [ ] Add thinking templates/examples for refactoring:
  - Understanding the current design
  - Identifying code smells
  - Planning incremental changes

#### 8.3 Update System Prompt
- [ ] Add "Domain-Specific Thinking Guides" section to system prompt
- [ ] Include examples for each identified domain
- [ ] Make it clear these are optional frameworks to consider

#### 8.4 Testing
- [ ] Verify the thinking guides don't overwhelm the prompt
- [ ] Test that the model appropriately uses domain-specific frameworks
- [ ] Gather feedback on reasoning quality improvements

---

## Task 9: Web Search Tool with SearXNG

### Overview
Implement a web search tool that uses a user-configured SearXNG instance. SearXNG is a self-hosted metasearch engine that provides privacy-respecting web search without sending data to commercial search providers.

### User Story
As a user, I want to search the web through my own SearXNG instance so that:
1. I can get search results while maintaining privacy
2. I avoid rate limits or costs from commercial search APIs
3. I have control over the search engine configuration

### Implementation Details

#### 9.1 Add SearXNG Configuration
- [ ] Add new config section in `Config`:
  ```rust
  #[serde(default)]
  pub searxng: Option<SearXngConfig>,
  
  #[derive(Debug, Deserialize, Clone, Default)]
  pub struct SearXngConfig {
      /// Base URL of the SearXNG instance (e.g., "https://searx.example.com")
      pub base_url: String,
      /// Enable/disable search (default: true)
      #[serde(default = "default_true")]
      pub enabled: bool,
  }
  
  fn default_true() -> bool {
      true
  }
  ```
- [ ] Add environment variables:
  - `SEARXNG_BASE_URL`
  - `SEARXNG_ENABLED`

#### 9.2 Create Search Tool
- [ ] Create new tool `src/tools/search.rs` implementing `rig::tool::Tool`
- [ ] Tool name: `search`
- [ ] Input schema:
  ```json
  {
    "type": "object",
    "properties": {
      "query": {
        "type": "string",
        "description": "The search query to send to SearXNG"
      },
      "num_results": {
        "type": "integer",
        "description": "Maximum number of results to return",
        "default": 10
      }
    },
    "required": ["query"]
  }
  ```
- [ ] Behavior:
  1. Send GET request to `{base_url}/search?q={query}&format=json`
  2. Parse JSON response
  3. Return formatted results with title, url, and snippet

#### 9.3 Error Handling
- [ ] Handle connection errors (instance unreachable)
- [ ] Handle SearXNG-specific errors (blocked, rate limited)
- [ ] Add timeout (default 30 seconds)
- [ ] Return helpful error messages

#### 9.4 Add Search Tool to Agent
- [ ] Import and add `SearchTool` to agent in `build_agent()`
- [ ] Only add if SearXNG is configured

#### 9.5 Update System Prompt
- [ ] Add section explaining when to use the search tool:
  ```
  ## Using the search tool
  
  Use the search tool to:
  - Find up-to-date information on topics
  - Look up documentation or references
  - Research solutions to problems
  - Get current information the model might not have
  
  Results come from your configured SearXNG instance.
  ```

#### 9.6 Testing
- [ ] Test connection to a SearXNG instance
- [ ] Test search query returns results
- [ ] Test error handling when instance is unreachable
- [ ] Test with various query types

---

## Task 10: LLM Timeout and Retry Handling

### Overview
Implement configurable timeout and automatic retry with exponential backoff for LLM API requests. Currently, if an API call times out or fails transiently, the user receives an error with no automatic recovery. Adding timeout and retry logic will improve reliability and user experience.

### Investigation Results

#### Existing rig-core functionality:
- rig-core has a retry module (`http_client/retry.rs`) with `ExponentialBackoff`, `Constant`, and `Never` policies
- These are primarily designed for SSE/event source reconnection, not completion request retries
- No built-in timeout configuration for LLM API requests in rig-core

#### Recommended approach:
Since rig-core doesn't expose retry logic for completion requests, implement timeout and retry at the application level in the REPL loop where `agent.prompt()` is called.

### Implementation Details

#### 10.1 Add LLM Configuration to Config
- [ ] Add new `llm` config section in `src/config.rs`:
  ```rust
  #[serde(default)]
  pub llm: Option<LlmConfig>,

  #[derive(Debug, Deserialize, Clone, Default)]
  pub struct LlmConfig {
      /// Request timeout in seconds (default: 120)
      #[serde(default = "default_llm_timeout")]
      pub timeout_seconds: u64,
      /// Maximum number of retries (default: 3)
      #[serde(default = "default_max_retries")]
      pub max_retries: u32,
      /// Initial retry delay in seconds (default: 1)
      #[serde(default = "default_retry_delay")]
      pub retry_delay_seconds: u64,
      /// Maximum retry delay in seconds (default: 30)
      #[serde(default = "default_max_retry_delay")]
      pub max_retry_delay_seconds: u64,
  }
  ```
- [ ] Add environment variables:
  - `LLM_TIMEOUT_SECONDS`
  - `LLM_MAX_RETRIES`
  - `LLM_RETRY_DELAY_SECONDS`
  - `LLM_MAX_RETRY_DELAY_SECONDS`

#### 10.2 Create Retry Logic Module
- [ ] Create `src/retry.rs` with:
  ```rust
  pub struct RetryConfig {
      pub max_retries: u32,
      pub initial_delay: Duration,
      pub max_delay: Duration,
      pub multiplier: f64,
  }

  impl RetryConfig {
      pub fn calculate_delay(&self, attempt: u32) -> Duration {
          // exponential backoff: initial_delay * (multiplier ^ attempt)
      }
  }

  /// Errors that should trigger a retry
  fn is_retryable_error(error: &rig::completion::CompletionError) -> bool {
      match error {
          // Network errors, timeouts, 5xx errors, rate limits (429), etc.
          CompletionError::Request(_) => true,  // Network issues
          CompletionError::ApiError(status, _) if status.as_u16() >= 500 => true,
          CompletionError::ApiError(status, _) if status.as_u16() == 429 => true,
          _ => false,
      }
  }
  ```

#### 10.3 Update AgentRunner to Use Retry Logic
- [ ] Modify `run()` method in `src/lib.rs` to:
  1. Wrap `agent.prompt()` call with retry logic
  2. Apply timeout using `tokio::time::timeout`
  3. On timeout: return user-friendly error, don't retry (user can increase timeout)
  4. On retryable error: wait with exponential backoff, then retry
  5. On non-retryable error: return immediately
  6. After max retries exhausted: return final error with retry count info

- [ ] Example flow:
  ```
  User input
      │
      ▼
  Attempt 1 ──► API call with timeout
      │
      ├─► Success ──► Return response
      │
      ├─► Timeout ──► Error (don't retry)
      │
      ├─► Retryable error ──► Wait (backoff) ──► Attempt 2
      │                                                  │
      │                                                  ├─► Success
      │                                                  │
      │                                                  ├─► Retryable ──► Wait ──► Attempt 3
      │                                                  │
      │                                                  └─► Max retries ──► Error
      │
      └─► Non-retryable error ──► Error
  ```

#### 10.4 Add User Feedback
- [ ] Show retry attempts in output:
  ```
  [Request failed, retrying in 2s... (attempt 2/3)]
  [Request failed, retrying in 4s... (attempt 3/3)]
  ```
- [ ] Add `/retry` command to manually retry last failed request
- [ ] Log retry attempts at debug/info level

#### 10.5 Configure Default Behavior
- [ ] Set sensible defaults:
  - Timeout: 120 seconds (LLM requests can be slow)
  - Max retries: 3
  - Initial delay: 1 second
  - Max delay: 30 seconds
  - Multiplier: 2x (exponential)

#### 10.6 Testing
- [ ] Test timeout triggers correctly
- [ ] Test retry on network errors
- [ ] Test no retry on non-retryable errors (auth failures, invalid requests)
- [ ] Test exponential backoff timing
- [ ] Test max retries limit
- [ ] Test with various error types from OpenRouter

#### 10.7 Documentation
- [ ] Update `agents.md` with LLM retry configuration
- [ ] Document which errors trigger retries

---

## Task 11: Fetch Markdown Tool

### Overview
Implement a tool to fetch web pages and convert them to Markdown format for efficient parsing and reading. Unlike the existing `fetch_url` tool that returns HTML, this tool uses a Markdown conversion library to provide clean, readable content.

### User Story
As a model, I want to fetch web pages as Markdown so that:
1. I can parse and extract information more efficiently
2. The content is cleaner without HTML noise
3. I can better understand document structure (headings, lists, etc.)

### Implementation Details

#### 11.1 Create FetchMarkdown Tool
- [ ] Create new tool `src/tools/fetch_markdown.rs` implementing `rig::tool::Tool`
- [ ] Tool name: `fetch_markdown`
- [ ] Input schema:
  ```json
  {
    "type": "object",
    "properties": {
      "url": {
        "type": "string",
        "description": "The URL to fetch and convert to Markdown"
      }
    },
    "required": ["url"]
  }
  ```

#### 11.2 Implement Markdown Conversion
- [ ] Use a crate like `html2md` or `pulldown-cmark` for conversion
- [ ] Add dependency to `Cargo.toml`:
  ```toml
  html2md = "0.3"
  ```
- [ ] Implement conversion:
  1. Fetch HTML via reqwest (like fetch_url)
  2. Convert HTML to Markdown using conversion library
  3. Return clean Markdown content

#### 11.3 Content Processing
- [ ] Remove scripts and styles
- [ ] Preserve headings (h1-h6)
- [ ] Preserve links and images (as Markdown syntax)
- [ ] Preserve code blocks with language hints
- [ ] Handle tables (convert to Markdown tables if possible)
- [ ] Truncate at 50,000 characters (like fetch_url)

#### 11.4 Add FetchMarkdown Tool to Agent
- [ ] Import and add `FetchMarkdownTool` to agent in `build_agent()`

#### 11.5 Update System Prompt
- [ ] Add section explaining when to use fetch_markdown:
  ```
  ## Using the fetch_markdown tool
  
  Use fetch_markdown instead of fetch_url when you need to:
  - Parse and extract information from web pages
  - Read documentation or articles
  - Analyze content structure
  
  Fetch_markdown returns clean Markdown that's easier to process than HTML.
  ```

#### 11.6 Testing
- [ ] Test fetching various HTML pages
- [ ] Verify Markdown conversion quality
- [ ] Test error handling for invalid URLs
- [ ] Test with pages that have complex HTML structures

---

## Task 12: Todo List Tool

### Overview
Implement a todo list tool that allows the model to track its own progress on multi-step tasks. The model can create tasks, update their status, and view the current state of its work plan.

### User Story
As a model, I want a todo list tool so that I can:
- Share my plan with the user before executing complex tasks
- Track progress as I work through multi-step problems
- Show what I've completed and what's remaining
- Keep the user informed about my work progress

### Implementation Details

#### 12.1 Create Todo Tool
- [ ] Create new tool `src/tools/todo.rs` implementing `rig::tool::Tool`
- [ ] Tool name: `todo`
- [ ] Input schema:
  ```json
  {
    "type": "object",
    "properties": {
      "action": {
        "type": "string",
        "enum": ["add", "update", "remove", "list", "clear"],
        "description": "The action to perform on the todo list"
      },
      "task": {
        "type": "string",
        "description": "Task description (for add/update)"
      },
      "status": {
        "type": "string",
        "enum": ["pending", "in_progress", "completed", "cancelled"],
        "description": "Task status (for update)"
      },
      "task_id": {
        "type": "integer",
        "description": "Task ID (for update/remove)"
      }
    },
    "required": ["action"]
  }
  ```

#### 12.2 Implement Todo State Management
- [ ] Create `TodoList` struct to manage tasks:
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize, Default)]
  pub struct TodoList {
      tasks: Vec<TodoItem>,
      next_id: usize,
  }
  
  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct TodoItem {
      pub id: usize,
      pub task: String,
      pub status: TodoStatus,
      pub created_at: DateTime<Utc>,
      pub updated_at: DateTime<Utc>,
  }
  
  #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
  #[serde(rename_all = "snake_case")]
  pub enum TodoStatus {
      Pending,
      InProgress,
      Completed,
      Cancelled,
  }
  ```
- [ ] Use a `Mutex<TodoList>` to maintain state across tool calls

#### 12.3 Implement Tool Actions
- [ ] **add**: Add a new task with "pending" status
  - Returns: "Added task #{id}: {task}"
- [ ] **update**: Change status of existing task
  - Validates task_id exists
  - Returns: "Updated task #{id} to {status}"
- [ ] **remove**: Delete a task by ID
  - Returns: "Removed task #{id}"
- [ ] **list**: Show all tasks with their status
  - Returns formatted list with completion summary
- [ ] **clear**: Remove all completed tasks
  - Returns: "Cleared {n} completed tasks"

#### 12.4 Add Todo Tool to Agent
- [ ] Import and add `TodoTool` to agent in `build_agent()`
- [ ] Ensure the todo state persists across the session

#### 12.5 Update System Prompt
- [ ] Add section explaining when to use the todo tool:
  ```
  ## Using the todo tool
  
  For multi-step tasks, use the todo tool to:
  - Plan your approach by adding tasks
  - Show the user what you intend to do
  - Track progress as you complete each step
  - Keep both yourself and the user informed
  
  Best practices:
  - Add tasks at the start of complex operations
  - Update status as you make progress
  - Use "in_progress" when starting work on a task
  - Use "completed" when finished, "cancelled" if abandoned
  
  Example workflow:
  1. Use todo add to create your plan
  2. Use todo list to show the user your plan
  3. As you work, update task statuses
  4. Use todo list to show progress
  ```

#### 12.6 Testing
- [ ] Test adding tasks
- [ ] Test updating task status
- [ ] Test removing tasks
- [ ] Test listing tasks with various statuses
- [ ] Test clearing completed tasks
- [ ] Test error cases (invalid task_id, etc.)

---

## Task 13: Conversation Persistence

### Overview
Implement the ability to save and restore conversation history, storing at every step so that if the program crashes or is interrupted, the conversation can be resumed with minimal loss. This includes automatic saving after each user input and each tool result.

### User Story
As a user, I want my conversation to be automatically saved so that:
1. I can resume my session after a crash or restart
2. I don't lose progress if the program is killed unexpectedly
3. I can manually save/load conversations for later review
4. I can list and manage previous conversations

### Implementation Details

#### 13.1 Define Conversation Storage Format
- [ ] Create `Conversation` struct in a new module `src/conversation.rs`:
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct Conversation {
      pub id: Uuid,
      pub name: String,
      pub created_at: DateTime<Utc>,
      pub updated_at: DateTime<Utc>,
      pub messages: Vec<Message>,
      pub model: String,
      pub metadata: ConversationMetadata,
  }

  #[derive(Debug, Clone, Serialize, Deserialize, Default)]
  pub struct ConversationMetadata {
      pub total_tokens: Option<u64>,
      pub total_cost: Option<f64>,
      pub message_count: usize,
  }

  #[derive(Debug, Clone, Serialize, Deserialize)]
  #[serde(tag = "role")]
  pub enum Message {
      User { content: String, timestamp: DateTime<Utc> },
      Assistant { content: String, timestamp: DateTime<Utc> },
      ToolResult { tool_name: String, result: String, timestamp: DateTime<Utc> },
  }
  ```
- [ ] Use JSON format for storage (human-readable, easy to debug)
- [ ] Store in platform data directory (e.g., `~/.local/share/peakbot/conversations/`)

#### 13.2 Create Conversation Manager
- [ ] Implement `ConversationManager` struct:
  ```rust
  pub struct ConversationManager {
      storage_dir: PathBuf,
      current_conversation: Option<Conversation>,
      auto_save: bool,
  }
  ```
- [ ] Implement methods:
  - `new(storage_dir, auto_save)` - create manager
  - `create_new(name, model)` - start a new conversation
  - `save()` - save current conversation to disk
  - `load(id)` - load a conversation by ID
  - `list()` - list all saved conversations
  - `delete(id)` - delete a conversation
  - `add_user_message(content)` - add user message and auto-save
  - `add_assistant_message(content)` - add assistant message and auto-save
  - `add_tool_result(tool_name, result)` - add tool result and auto-save

#### 13.3 Implement Auto-Save
- [ ] Modify REPL loop to save after every significant event:
  - After receiving user input (before sending to model)
  - After receiving assistant response
  - After each tool result
- [ ] Use write-ahead logging or atomic writes to prevent corruption:
  ```rust
  fn save_atomic(&self, conversation: &Conversation) -> Result<()> {
      let temp_path = self.storage_dir.join(".tmp.json");
      let final_path = self.storage_dir.join(format!("{}.json", conversation.id));
      
      let json = serde_json::to_string_pretty(conversation)?;
      std::fs::write(&temp_path, json)?;
      std::fs::rename(&temp_path, &final_path)?;
      Ok(())
  }
  ```
- [ ] Add crash recovery: on startup, check for incomplete saves and recover

#### 13.4 Add Resume Functionality
- [ ] On startup, check for the most recent conversation
- [ ] Prompt user to resume or start new:
  ```
  Found saved conversation: "My Project"
  Last activity: 2 hours ago
  15 messages
  
  Resume [r], Start new [n], List all [l]? 
  ```
- [ ] Implement `/resume` command in REPL to manually resume a conversation
- [ ] Implement `/new` command to start a fresh conversation

#### 13.5 Add Conversation Management Commands
- [ ] Add REPL commands:
  - `/save [name]` - save current conversation with optional name
  - `/load <id>` - load a conversation by ID
  - `/conversations` or `/history` - list all saved conversations
  - `/delete <id>` - delete a conversation
  - `/export <id> <format>` - export conversation (json, markdown)
  - `/rename <name>` - rename current conversation
- [ ] Add tab completion for conversation IDs

#### 13.6 Add Configuration Options
- [ ] Add to config:
  ```yaml
  conversation:
    # Enable auto-save (default: true)
    auto_save: true
    # Storage directory (default: platform data dir)
    storage_dir: ~/.local/share/peakbot/conversations
    # Maximum conversations to keep (default: 50, 0 = unlimited)
    max_conversations: 50
    # Auto-load last conversation on startup (default: true)
    auto_resume: true
  ```
- [ ] Add environment variables:
  - `CONVERSATION_AUTO_SAVE`
  - `CONVERSATION_STORAGE_DIR`
  - `CONVERSATION_AUTO_RESUME`

#### 13.7 Update AgentRunner for Persistence
- [ ] Modify `AgentRunner` to hold a `ConversationManager`
- [ ] After each agent.prompt() cycle, add messages to conversation and save
- [ ] On startup, initialize conversation manager and optionally resume

#### 13.8 Testing
- [ ] Test auto-save after user input
- [ ] Test auto-save after assistant response
- [ ] Test auto-save after tool result
- [ ] Test resume after crash simulation
- [ ] Test conversation listing
- [ ] Test export to different formats
- [ ] Test concurrent write safety
- [ ] Test recovery from partial writes

---

## Dependencies and Ordering

Some tasks have dependencies on others:
| Status | Task | Depends On |
|--------|------|------------|
| [ ] | 1 (Dynamic MCP) | - |
| [x] | 2 (Think Tool) | - |
| [x] | 3 (Token Hook) | - |
| [ ] | 4 (Logging Hooks) | 3 (uses PromptHook) |
| [x] | 5 (Ollama) | - |
| [ ] | 6 (Debug ApiResponse) | - |
| [x] | 7 (Context Compaction) | 3 (token counting) |
| [ ] | 8 (Domain Thinking) | 2 (think tool) |
| [x] | 9 (SearXNG Search) | - |
| [ ] | 10 (Fetch Markdown) | - |
| [ ] | 11 (Todo Tool) | - |
| [ ] | 13 (Conversation Persistence) | - |

**Recommended implementation order:**
1. Task 6 (Debug ApiResponse) - Fix existing bug
2. Task 2 (Think Tool) - Simple, good warm-up
3. Task 3 (Token Counting) - Foundation for 4 and 7
4. Task 4 (Logging Refactor) - Benefits from 3
5. Task 7 (Context Compaction) - Uses token counting from 3
6. Task 1 (Dynamic MCP) - Independent but uses similar patterns
7. Task 5 (Ollama) - Independent, can be done anytime
8. Task 8 (Domain-specific thinking) - Independent
9. Task 9 (SearXNG Search) - Independent, uses existing HTTP patterns
10. Task 10 (Fetch Markdown) - Independent, uses existing HTTP patterns
11. Task 11 (Todo Tool) - Independent, simple tool
12. Task 13 (Conversation Persistence) - Independent, valuable for UX

---

## Future Considerations

After completing these tasks, consider:
- **Extended Thinking**: Anthropic recommends extended thinking for simpler cases (can be enabled via API params for Claude models)
- **Multiple Concurrent Ollama**: Support connecting to multiple Ollama instances
- **MCP Server Hot-Reload**: Watch config for changes and dynamically update
- **Token Budget/limits**: Add max tokens per session or per-request limits
- **Custom Hooks**: Allow users to provide custom PromptHook implementations
- **Metrics Export**: Export statistics to Prometheus, OpenTelemetry, etc.
