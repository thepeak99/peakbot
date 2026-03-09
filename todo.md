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

#### 2.4 Optional: Extended Prompt for Specific Domains
- [ ] Consider adding domain-specific thinking examples to system prompt
- [ ] This follows Anthropic's recommendation for complex domains

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
- [ ] Create new module `src/hooks/mod.rs`
- [ ] Create `TokenCostHook` struct implementing `PromptHook<M>`
- [ ] Define model pricing (can be expanded later):
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
- [ ] Implement `on_completion_call` to log/track request start
- [ ] Implement `on_completion_response` to:
  - Extract token usage from response
  - Calculate costs
  - Log/display metrics
- [ ] Implement `on_tool_call` and `on_tool_result` for detailed tool logging

#### 3.3 Create Statistics Struct
- [ ] Create `SessionStats` to track:
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
- [ ] After each model response, display:
  ```
  [Tokens: {input} in / {output} out | Cost: ${cost} | Total: ${total}]
  ```
- [ ] Add command to show cumulative stats (e.g., `/stats`)

#### 3.5 Make Hook Pluggable
- [ ] Design `TokenCostHook` to be optional
- [ ] Add config option to enable/disable cost tracking
- [ ] Allow custom pricing via config file

#### 3.6 Testing
- [ ] Test hook is called on each request
- [ ] Test token calculation accuracy
- [ ] Test cost calculation with known prices

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
Implement automatic context management to handle conversations that grow too large. When the conversation context (chat history) approaches the model's context window limit, the system should compact or summarize older messages to make room for new ones.

### User Story
As a user, I want to have long conversations without hitting context window limits, so that:
1. I can work on complex tasks that require many back-and-forth exchanges
2. The agent maintains context across extended sessions
3. I don't lose access to important parts of the conversation history

### Implementation Details

#### 7.1 Design Context Management Strategy
- [ ] Define context window threshold (e.g., 80% of max tokens)
- [ ] Decide on compaction strategy:
  - **Summarization**: Use the model to summarize older messages
  - **Truncation**: Simply remove oldest messages
  - **Hybrid**: Summarize some, truncate some
- [ ] Determine what to preserve (system prompt, tool definitions, etc.)

#### 7.2 Create Context Manager Struct
- [ ] Create `src/context_manager.rs`
- [ ] Implement `ContextManager` with:
  ```rust
  pub struct ContextManager {
      max_tokens: usize,
      threshold_percent: f64,
      system_prompt_tokens: usize,
  }
  ```
- [ ] Implement methods:
  - `new(max_tokens, system_prompt_tokens)` - create manager
  - `needs_compaction(messages)` - check if compaction needed
  - `compact(messages, model)` - perform compaction
  - `estimate_tokens(messages)` - estimate token count

#### 7.3 Implement Token Estimation
- [ ] Create simple token counter (can use tiktoken or approximation)
- [ ] Count tokens for messages, system prompt, tool definitions
- [ ] Account for overhead in API request (formatting, tool schemas)

#### 7.4 Implement Compaction Logic
- [ ] **Truncation approach** (simpler):
  - Calculate how many tokens need to be freed
  - Remove oldest messages until under threshold
  - Keep at least last N messages for context
  
- [ ] **Summarization approach** (better):
  - Identify messages to summarize (e.g., older than X turns)
  - Use model to generate summary
  - Replace original messages with summary + key info
  - Preserve user tool outputs if important

#### 7.5 Integrate with AgentRunner
- [ ] Modify REPL loop to check context size before each request
- [ ] Call compaction when needed, before sending to model
- [ ] Inform user when compaction occurs:
  ```
  [Context compacted: summarized 15 messages into 3]
  ```

#### 7.6 Preserve Important Context
- [ ] Never remove system prompt
- [ ] Keep recent tool definitions (if dynamically added)
- [ ] Preserve last N user/assistant exchanges
- [ ] Handle special messages (enable_mcp results, etc.)

#### 7.7 Add User Controls
- [ ] Add config options:
  ```yaml
  context:
    # Compaction threshold (0.0-1.0), default 0.8
    threshold: 0.8
    # Compaction strategy: "truncate" or "summarize"
    strategy: "truncate"
    # Keep last N messages always
    keep_recent: 5
    # Enable/disable compaction
    enabled: true
  ```
- [ ] Add `/compact` command for manual compaction
- [ ] Add `/context` command to show context usage

#### 7.8 Testing
- [ ] Test token estimation accuracy
- [ ] Test truncation preserves recent messages
- [ ] Test summarization maintains key information
- [ ] Test edge cases (empty history, very long messages)
- [ ] Test with various model context sizes

---

## Dependencies and Ordering

Some tasks have dependencies on others:

| Task | Depends On |
|------|------------|
| 1 (Dynamic MCP) | - |
| 2 (Think Tool) | - |
| 3 (Token Hook) | - |
| 4 (Logging Hooks) | 3 (uses PromptHook) |
| 5 (Ollama) | - |
| 6 (Debug ApiResponse) | - |
| 7 (Context Compaction) | 3 (token counting) |

**Recommended implementation order:**
1. Task 6 (Debug ApiResponse) - Fix existing bug
2. Task 2 (Think Tool) - Simple, good warm-up
3. Task 3 (Token Counting) - Foundation for 4 and 7
4. Task 4 (Logging Refactor) - Benefits from 3
5. Task 7 (Context Compaction) - Uses token counting from 3
6. Task 1 (Dynamic MCP) - Independent but uses similar patterns
7. Task 5 (Ollama) - Independent, can be done anytime

---

## Future Considerations

After completing these tasks, consider:
- **Extended Thinking**: Anthropic recommends extended thinking for simpler cases (can be enabled via API params for Claude models)
- **Multiple Concurrent Ollama**: Support connecting to multiple Ollama instances
- **MCP Server Hot-Reload**: Watch config for changes and dynamically update
- **Token Budget/limits**: Add max tokens per session or per-request limits
- **Custom Hooks**: Allow users to provide custom PromptHook implementations
- **Metrics Export**: Export statistics to Prometheus, OpenTelemetry, etc.