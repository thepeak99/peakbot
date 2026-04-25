# PeakBot Documentation

## Overview

PeakBot is a single-agent coding assistant built with [Rig](https://github.com/0xPlaygrounds/rig) (`rig-core` v0.33). It runs as a terminal REPL, equipped with filesystem, shell, web fetch, and web search tools. It also supports dynamically loading tools from MCP (Model Context Protocol) servers and Agent Skills. Additional features include conversation persistence, todo list management, and an event-driven hooks system for cost tracking.

## Architecture

```
User (stdin)
  │
  ▼
┌──────────────────────────────────────────────┐
│  REPL Loop  (src/main.rs / src/lib.rs)      │
│  Reads input, passes to agent, prints output │
│  Commands: /stats, /context, /compact,       │
│           /conversations                     │
│  Maintains:                                  │
│  - chat_history: Vec<Message>                │
│  - todo_state: Arc<Mutex<TodoList>>          │
└──────────────┬───────────────────────────────┘
               │
               ▼
┌──────────────────────────────────────────────┐
│  Provider Abstraction (src/providers/)       │
│  ┌────────────────────────────────────────┐  │
│  │  DynAgent (enum for runtime switching) │  │
│  │  ┌──────────────────────────────────┐  │  │
│  │  │ OpenRouter  │  OpenAI   │ LlamaCpp│  │  │
│  │  │ (100+ models) │ (GPT APIs) │(local) │  │  │
│  │  └──────────────────────────────────┘  │  │
│  │  ┌──────────────────────────────────┐  │  │
│  │  │   Ollama (local models)          │  │  │
│  │  └──────────────────────────────────┘  │  │
│  │                                        │  │
│  │  CostTracker: Unified cost tracking    │  │
│  │  SessionHook: Event emission           │  │
│  └────────────────────────────────────────┘  │
└──────────────┬───────────────────────────────┘
               │
               ▼
┌──────────────────────────────────────────────┐
│  Context Manager (src/context_manager.rs)    │
│  Auto-compacts context when threshold reached│
│  Uses actual token counts from provider      │
│  Passes system_prompt to agent for           │
│  summarization-based compaction              │
└──────────────┬───────────────────────────────┘
               │
               ▼
┌──────────────────────────────────────────────┐
│  Rig Agent                                   │
│  System prompt: built from + skills + env    │
│  Max tokens: configurable                    │
│  Max tool turns per message: configurable    │
│  Tools: built-in (8) + MCP + todo tool       │
└──────────────┬───────────────────────────────┘
               │
               ▼
┌──────────────────────────────────────────────┐
│  Event Channel (async)                       │
│  ┌────────────────────────────────────────┐  │
│  │ SessionHook emits: AgentEvent          │  │
│  │  - CompletionResponse (tokens, cost)   │  │
│  │  - ToolCall, ToolResult                │  │
│  └────────────────┬───────────────────────┘  │
│                   │                           │
│                   ▼                           │
│  ┌────────────────────────────────────────┐  │
│  │ AgentRunner processes events →         │  │
│  │ updates CostTracker (external)         │  │
│  └────────────────────────────────────────┘  │
└──────────────────────────────────────────────┘
               │
               ▼
┌──────────────────────────────────────────────┐
│  Conversation Manager (src/conversation_     │
│  manager.rs)                                 │
│  Auto-saves conversations to JSON files      │
│  Supports auto-resume, listing, metadata     │
│  Stores: User, Assistant, ToolCall, ToolResult│
└──────────────────────────────────────────────┘
```

## Core Components

### Provider Abstraction (`src/providers/mod.rs`)

PeakBot supports multiple LLM providers through a unified provider abstraction:

| Provider | Features | Cost Tracking | API Type |
|----------|----------|---------------|----------|
| **OpenRouter** | Access to 100+ models via API | ✅ Full support | Completion API |
| **OpenAI** | Direct access to GPT models, configurable endpoint | ✅ Full support | Responses API |
| **LlamaCpp** | llama.cpp compatible endpoints | ✅ Supported | Completion API |
| **Ollama** | Local models (llama3, qwen, mistral, etc.) | ❌ Not supported | Completion API |

The provider system provides:
- `DynAgent` - Dynamic enum for runtime provider switching
- `CostTracker` - Unified interface for session statistics
- `ProviderInfo` - Metadata (name, model, pricing support)

**DynAgent Variants**:
- `OpenRouter(Agent<CompletionModel, SessionHook>)` - Full cost tracking
- `OpenAI(Agent<ResponsesCompletionModel, SessionHook>)` - Uses OpenAI's responses API
- `LlamaCpp(Agent<CompletionModel, SessionHook>)` - llama.cpp compatible
- `Ollama(Agent<CompletionModel, ()>)` - No hook for local models

### Context Manager (`src/context_manager.rs`)

Manages long conversations by automatically compacting context when approaching the context window limit:

- **Threshold**: Triggers compaction at 80% context usage (configurable)
- **Keep Recent**: Always preserves last N messages (default: 5)
- **Auto-detection**: Automatically detects context window size from model name
- **Methods**: Compression via summarization or message collapsing

Supported context windows (auto-detected):
- Claude 3.7/3.5/3 Opus/Sonnet/Haiku: 200k
- GPT-4o: 128k
- Gemini 2.0: 1M
- Gemini 1.5 Pro/Flash: 1M-2M

### Hooks System (`src/hooks/`)

Extends the agent with event-driven post-processing capabilities:

**Module Structure**:
- `session_hook.rs` - Main hook implementation
  - `SessionHook` - Implements `PromptHook` for emitting events
  - `SessionStats` - Accumulates total tokens, requests, and cost
  - `ModelPricing` - Per-model pricing data (input/output per token)
  - `fetch_model_pricing()` - Fetches pricing from OpenRouter API
- `channel.rs` - Async event channel
  - `EventChannel` - Creates event channel and processor
  - `EventProcessor` - Processes events with handlers
- `events.rs` - Event types
  - `AgentEvent` - Enum for LLM events (CompletionResponse, ToolCall, ToolResult)
  - `TokenUsage` - Token usage data structure

**Cost Tracking Flow**:
1. `SessionHook` emits `AgentEvent` for each LLM call
2. Events are sent via async channel to `AgentRunner`
3. `CostTracker` processes events and calculates costs
4. Session stats are updated and accessible via REPL commands

**Key Types**:
- `CostTracker` - External cost tracking handle (not a hook itself)
- `SessionStats` - Thread-safe statistics with `Arc<Mutex<>>`
- `ModelPricing` - Fetched from OpenRouter API or uses defaults

### Skills System (`src/skills/`)

Agent Skills are modular capability packages that extend PeakBot's functionality:

**Discovery** (`src/skills/discovery.rs`):
- Loads skills from `~/.agents/skills` (global) and `./.agents/skills` (local)
- Each skill is a directory containing a `SKILL.md` file
- Skills are dynamically added to the system prompt

**Skill Structure**:
```
.agents/skills/my-skill/
├── SKILL.md          # Required: describes capability
├── scripts/          # Optional: executable scripts
├── references/       # Optional: documentation
└── assets/           # Optional: files/images
```

## Agent Construction

The agent is built in `src/lib.rs` via `create_provider()` in `src/providers/mod.rs`:

```rust
pub fn create_provider(
    config: &ProviderConfig,
    mcp_tools: Option<Vec<Box<dyn ToolDyn>>>,
    system_prompt: &str,
    searxng_config: Option<&SearXngConfig>,
    max_turns: usize,
    todo_tool: Option<TodoTool>,
    bash_config: &BashConfig,
) -> Result<(DynAgent, ProviderInfo, CostTracker, Arc<Mutex<TodoList>>, Option<mpsc::UnboundedReceiver<AgentEvent>>)>
```

**Return Values**:
- `DynAgent` - The configured agent for the selected provider
- `ProviderInfo` - Metadata about the provider (name, model, pricing support)
- `CostTracker` - Handle for accessing session statistics and costs
- `Arc<Mutex<TodoList>>` - Shared state for the todo tool
- `Option<mpsc::UnboundedReceiver<AgentEvent>>` - Event channel for external processing

**Agent Initialization Flow** (`src/main.rs`):
1. Load configuration from environment variables and `config.yaml`
2. Load skills from `~/.agents/skills` and `./.agents/skills`
3. Build system prompt dynamically (includes skills, cwd, time, agents.md content)
4. Load MCP servers (async) and extract their tools
5. Create `TodoTool` instance
6. Call `create_provider()` with all components
7. Create `AgentRunner` with agent, config, and shared state
8. Run the REPL loop

**State Management**:
- **Conversation memory**: `chat_history` in `AgentRunner` (Vec<Message>)
- **Todo state**: Shared `Arc<Mutex<TodoList>>` accessible by the agent
- **File edit history**: `FileEditTool.file_history` (undo stack, not yet exposed)
- **Session stats**: `CostTracker` with `SessionStats` (Arc<Mutex<>>)
- **Events**: Processed externally by `AgentRunner` from the channel

The agent is stateless between tool turns -- Rig owns the agentic loop: it automatically dispatches tool calls, collects results, and loops until the model produces a text response or hits the 50-turn limit.

## Tools

All tools live in `src/tools/` and implement `rig::tool::Tool`. Each tool defines:
- `NAME` -- unique string identifier sent to the model
- `Args` -- struct deserialized from JSON the model produces
- `Output` -- type serialized back to the model as a tool result
- `Error` -- tool-specific error type (errors are sent back to the model, not fatal)
- `definition()` -- returns the JSON Schema the model uses to know what to send
- `call()` -- executes the tool logic

PeakBot includes **8 built-in tools**:

### Tools Overview

PeakBot includes **8 built-in tools** (all always available):

| Tool | File | Description |
|------|------|-------------|
| `file_edit` | `file_edit.rs` | Create, replace, insert text in files |
| `file_read` | `file_read.rs` | Read files with line ranges |
| `list_directory` | `list_directory.rs` | List directory contents with recursion |
| `bash` | `bash.rs` | Execute shell commands with timeout, truncate to last 50k chars, save full output to temp |
| `fetch_url` | `fetch_url.rs` | HTTP GET requests to URLs |
| `web_search` | `search.rs` | SearXNG-based web search |
| `think` | `think.rs` | Reasoning tool for complex thinking |
| `todo` | `todo.rs` | Todo list management |

Plus optional **MCP tools** from configured servers.

## Data Flow

1. **User types a message** in the terminal.
2. AgentRunner checks context usage and optionally triggers compaction
3. `agent.prompt_with_history(input, &mut chat_history)` enters the agentic loop
4. The model responds with either text (done) or tool calls.
5. For each tool call:
   - Rig deserializes `Args` from the JSON the model produced
   - Calls `tool.call(args)`
   - Serializes `Output` back to JSON
6. Tool results are appended to the conversation as `ToolResult` messages
7. `SessionHook` emits `AgentEvent` for each LLM call (tokens, cost)
8. Events are sent via channel to `AgentRunner`
9. `CostTracker` processes events and updates session statistics
10. Results are printed and session stats are updated

## Error Handling

- **Tool errors** (bad path, non-unique match, timeout) are returned to the model as tool results. The model sees the error message and self-corrects. These do not crash the agent.
- **API errors** (auth failure, network) surface as `PromptError` and are printed in the REPL. The loop continues.
- **Max turns exceeded** means the model used all 50 tool rounds without producing a final answer. Printed as an error; the user can retry or rephrase.

## Configuration

PeakBot supports multiple LLM providers (OpenRouter, OpenAI, LlamaCpp, Ollama). Configuration is loaded from `config.yaml` in the platform config directory, with environment variables taking precedence.

### Provider Configuration

The recommended config format uses the `provider` key:

```yaml
# OpenRouter example (100+ models)
provider:
  type: openrouter
  config:
    api_key: sk-or-v1-xxx
    model: anthropic/claude-3.7-sonnet
    max_tokens: 4096

# OpenAI example (direct API access with configurable endpoint)
provider:
  type: openai
  config:
    api_key: sk-xxx
    base_url: https://api.openai.com/v1  # Default - can be overridden for Azure, local proxies
    model: gpt-4o
    max_tokens: 4096

# LlamaCpp example (llama.cpp server with OpenAI-compatible API)
provider:
  type: llamacpp
  config:
    api_key: optional  # Optional for local instances
    base_url: http://localhost:8080
    model: llama3
    max_tokens: 4096

# Ollama example (local models)
provider:
  type: ollama
  config:
    base_url: http://localhost:11434
    model: llama3
    temperature: 0.7
    num_ctx: 4096
```

### Additional Configuration Options

```yaml
# Bash tool configuration (environment variables for shell commands)
# Note: bash output is truncated to ~50k chars (keeping the end like `tail`).
# Full output is saved to /tmp/peakbot/ and can be accessed via file_read.
bash:
  env:
    MY_API_KEY: "secret-key-123"
    MY_CUSTOM_PATH: "/opt/custom/bin"
    # These env vars will be available in all bash command executions

# SearXNG web search configuration
searxng:
  base_url: https://searx.example.com
  enabled: true
  timeout_seconds: 30
  max_results: 10

# Context compaction settings
context:
  enabled: true
  threshold: 0.8          # Trigger at 80% context usage
  keep_recent: 5          # Always keep last 5 messages
  context_window: 200000  # Or 0 for auto-detect

# Token cost tracking (OpenRouter only)
cost_tracking: true

# MCP servers
mcp_servers:
  - name: filesystem
    command: "npx"
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]
```

### Legacy Configuration (Backward Compatible)

The old format is still supported:

```yaml
# Legacy format (still works)
openrouter_api_key: sk-or-v1-xxx
openrouter_model: anthropic/claude-3.7-sonnet
openrouter_max_tokens: 4096
```

### Environment Variables

| Environment Variable | Description |
|---------------------|-------------|
| `PROVIDER` | JSON provider config (new format) |
| `OPENROUTER_API_KEY` | OpenRouter API key (legacy) |
| `OPENROUTER_MODEL` | OpenRouter model (legacy) |
| `OPENROUTER_MAX_TOKENS` | Max tokens for OpenRouter (legacy) |
| `OPENAI_API_KEY` | OpenAI API key |
| `OPENAI_BASE_URL` | OpenAI base URL (legacy) |
| `LLAMACPP_API_KEY` | LlamaCpp API key (legacy) |
| `LLAMACPP_BASE_URL` | LlamaCpp base URL (legacy) |
| `LLAMACPP_MODEL` | LlamaCpp model name (legacy) |
| `OLLAMA_MODEL` | Ollama model name (legacy) |
| `OLLAMA_BASE_URL` | Ollama base URL (legacy) |
| `OLLAMA_TEMPERATURE` | Ollama temperature (legacy) |
| `OLLAMA_NUM_CTX` | Ollama context size (legacy) |
| `AGENT_MAX_TURNS` | Max tool turns per message |
| `MCP_SERVERS` | JSON array of MCP server configs |
| `SEARXNG_BASE_URL` | SearXNG base URL |

### REPL Commands

| Command | Description |
|---------|-------------|
| `/stats` | Show session statistics (tokens, cost) |
| `/context` | Show context usage status |
| `/compact` | Force context compaction |
| `exit` | Quit the REPL |

### Settings

| Setting | Default | Description |
|---------|---------|-------------|
| Model | `anthropic/claude-3.7-sonnet` | LLM model to use |
| Max tokens | 4096 | Maximum tokens in response |
| Max tool turns | 50 | Tool calls per prompt |
| Output truncation | 10,000 chars | File tool output limit |
| Bash timeout | 30s | Shell command timeout |

## Extending

### Adding a new built-in tool

1. Create `src/tools/my_tool.rs`
2. Define `MyToolArgs` (derive `Deserialize`), `MyToolError` (derive `thiserror::Error`), and `MyTool` (derive `Serialize, Deserialize`)
3. Implement `rig::tool::Tool` for `MyTool`:
   - Set `NAME`, `Error`, `Args`, `Output`
   - Return a `ToolDefinition` with JSON Schema in `definition()`
   - Implement logic in `call()`
4. Add `mod my_tool;` and `pub use my_tool::MyTool;` in `src/tools/mod.rs`
5. Add `.tool(MyTool)` to the agent builder in `src/providers/mod.rs:add_builtin_tools()`

### Adding Agent Skills

Instead of building tools into the binary, create a skill package:

1. Create a directory in `~/.agents/skills/` or `./.agents/skills/`
2. Add a `SKILL.md` file describing:
   - When to use the skill
   - Required inputs
   - Usage instructions
3. Optionally add scripts/, references/, and assets/ subdirectories
4. Skills are automatically discovered and loaded at startup

### Adding MCP tools

Instead of building tools into the binary, you can configure external MCP (Model Context Protocol) servers. Tools from MCP servers are dynamically loaded at runtime:

```yaml
# config.yaml
mcp_servers:
  - name: filesystem
    command: "npx"
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]
```

Or via environment variable (JSON format):
```bash
export MCP_SERVERS='[{"name":"filesystem","command":"npx","args":["-y","@modelcontextprotocol/server-filesystem","/path/to/dir"]}]'
```

MCP tools are automatically wrapped with `LoggingToolDyn` for tracing.

### Switching models

Change the model in `config.yaml` or set the `OPENROUTER_MODEL` environment variable. Find models at https://openrouter.ai/models.

Example models:
- `anthropic/claude-3.7-sonnet` (default)
- `anthropic/claude-3.5-sonnet`
- `google/gemini-2.0-flash-001`
- `openai/gpt-4o`
- `qwen/qwq-32b`

### Multi-agent patterns

Rig supports agent-as-tool: any `Agent<M>` implements `Tool`. To add a sub-agent:

```rust
let researcher = client
    .agent(CLAUDE_3_5_HAIKU)
    .preamble("You research codebases and summarize findings.")
    .name("researcher")
    .build();

let main_agent = client
    .agent(CLAUDE_4_SONNET)
    .preamble("You are a coding agent. Use the researcher for broad codebase questions.")
    .tool(researcher)  // sub-agent as a tool
    .tool(FileEditTool::default())
    .tool(BashTool)
    .build();
```

## File Map

```
src/
├── main.rs                 # Entry point, creates AgentRunner
├── lib.rs                  # AgentRunner, system prompt building, conversation conversion
├── config.rs               # Configuration loading from config.yaml + env vars
├── context_manager.rs      # Context compaction for long conversations
├── conversation.rs         # Conversation data structures
├── conversation_manager.rs # Conversation persistence manager
├── system_prompt.txt       # Base system prompt (included at compile time)
├── providers/
│   └── mod.rs              # Provider abstraction (OpenRouter, OpenAI, LlamaCpp, Ollama), DynAgent, CostTracker
├── hooks/
│   ├── mod.rs              # Hook exports
│   ├── session_hook.rs     # SessionHook, SessionStats, ModelPricing, fetch_model_pricing
│   ├── channel.rs          # EventChannel, EventProcessor
│   └── events.rs           # AgentEvent, TokenUsage
├── skills/
│   ├── mod.rs              # Skills module exports
│   ├── discovery.rs        # Skill discovery and SkillRegistry
│   ├── parser.rs           # SKILL.md parsing
│   └── types.rs            # Skill data structures
├── ui/
│   ├── mod.rs              # UI module exports
│   ├── app_state.rs        # AppState (single source of truth for UIs)
│   ├── ui_trait.rs         # Ui trait + UiAction enum
│   └── repl/
│       ├── mod.rs              # REPL module exports
│       ├── message_renderer.rs # MessageRenderer, PlainRenderer
│       ├── render_cache.rs     # ChatRenderCache, WindowView
│       ├── repl_impl.rs        # ReplUi View (ratatui) + render loop
│       ├── spinner.rs          # Working-indicator spinner frames + elapsed formatter
│       └── todo_panel.rs       # TODO side-panel renderer
└── tools/
    ├── mod.rs              # Re-exports: all built-in tools
    ├── bash.rs             # BashTool -- shell execution with timeout
    ├── fetch_url.rs        # FetchUrlTool -- HTTP GET requests
    ├── file_edit.rs        # FileEditTool -- create/str_replace/insert
    ├── file_read.rs        # FileReadTool -- read with line ranges
    ├── list_directory.rs   # ListDirectoryTool -- dir listing with recursion
    ├── search.rs           # SearchTool -- SearXNG web search
    ├── think.rs            # ThinkTool -- reasoning tool
    ├── todo.rs             # TodoTool -- todo list management
    └── logging_wrapper.rs  # LoggingToolDyn -- wrapper for MCP tool tracing
└── tests/
    ├── integration.rs         # Test module entry point
    ├── harness/               # Test utilities (TestHarness, mock providers)
    ├── mock/                  # MockCompletionModel for testing
    ├── scenarios/              # Test scenarios
    │   ├── message_roundtrip.rs
    │   ├── stats_tests.rs
    │   └── tool_tests.rs
    └── storage/               # Storage implementations for tests
```

## Testing

PeakBot includes a comprehensive integration test suite in `tests/integration.rs` that uses a `MockCompletionModel` for testing without requiring API calls.

### Test Architecture

**MockCompletionModel** (`tests/mock/`): A mock implementation of Rig's `CompletionModel` trait that returns pre-configured responses:
- Supports text responses, tool calls, and streamed responses
- Configurable delay simulation for async operations
- Tracks all requests for verification

**ConversationStorage Trait** (`tests/storage/`): Abstract storage interface for conversation persistence:
- `InMemoryStorage` - Default implementation for tests
- Enables testing conversation save/load without filesystem

**TestHarness** (`tests/harness/`): Test utility that creates an agent with:
- MockCompletionModel for LLM responses
- InMemoryStorage for conversation persistence
- Isolated todo state per test

**Test Scenarios** (`tests/scenarios/`):
- `message_roundtrip.rs` - Message flow through the agent
- `stats_tests.rs` - Cost tracking and statistics
- `tool_tests.rs` - Tool functionality tests

### Running Tests

```bash
# Run all tests
cargo test

# Run integration tests only
cargo test --test integration

# Run with output
cargo test -- --nocapture
```

### Test Categories

| Category | Tests | Description |
|----------|-------|-------------|
| **Message Roundtrips** | Basic roundtrip, multi-turn, system prompt preservation | Tests that messages flow correctly through the agent |
| **Stats Tracking** | Cost tracking, token counting, sequential requests | Verifies cost tracking works correctly |
| **Storage Operations** | Save/load conversations, clear history | Tests conversation persistence |
| **Todo Tool** | Add, update, remove, list todos | Direct TodoTool functionality |
| **Error Handling** | Empty responses, invalid inputs | Edge case handling |

### Writing New Tests

```rust
#[test]
fn my_new_test() {
    let harness = TestHarness::new()
        .with_text_response("Hello!")
        .build();
    
    let (output, _) = harness.run_prompt("Say hello");
    assert!(output.contains("Hello!"));
}
```

For testing specific provider behavior or error conditions, use the builder pattern:

```rust
let harness = TestHarness::new()
    .with_streamed_response(vec!["Part ", "one"])
    .with_conversation_id("test-session")
    .build();
```

## Commit Procedure

**Every commit must pass clean `cargo fmt` and `cargo clippy` before it lands.**
This is non-negotiable — the build pipeline treats clippy warnings as
real signals, the release flow rebuilds three platforms, and stale
warnings hide real ones. Run the gate locally so CI doesn't have to
catch you.

### Pre-commit gate

Run these three commands in order from the repo root before *any*
`git commit`:

```bash
cargo fmt --all                                       # 1. format
cargo clippy --all-targets --all-features -- -D warnings   # 2. lint, no warnings allowed
cargo test                                            # 3. tests still pass
```

If clippy fires, **fix it** rather than allow it. The two acceptable
escape hatches are:

1. A scoped `#[allow(clippy::lint_name)]` on the offending item with a
   one-line comment explaining *why* the lint is wrong here. Example
   in `src/providers/mod.rs`: provider constructors deliberately have
   many args + complex return types — refactoring to a builder gains
   nothing, so the module carries `#![allow(clippy::too_many_arguments,
   clippy::type_complexity)]` with a doc-comment explaining the choice.
2. A `#[allow(dead_code)]` on test utilities kept for future scenarios,
   with a comment naming the future use.

Never blanket-allow at crate level. Never silence warnings without a
comment. If you can't justify the silence in one line, fix the code.

### Snapshot tests

`tests/repl_tests.rs` uses `insta` for golden-file rendering tests.
After a version bump or any deliberate UI change, snapshots will fail
with `.snap.new` files written next to the originals. Review the diff,
then either:

- `cargo insta review` (interactive, recommended) — accept/reject each
- `mv tests/snapshots/<name>.snap.new tests/snapshots/<name>.snap` —
  manual accept of a single snapshot when you've already eyeballed it

Never commit `.snap.new` files; they'll fail CI.

### Why this matters

- The release flow (`make release`) compiles three platforms from
  scratch. A warning that compiles fine on Linux can be a hard error on
  Windows or macOS.
- The `-D warnings` gate means *any* new warning blocks the commit.
  This forces the warning to be either fixed or explicitly allowed at
  the moment it's introduced — not three releases later when nobody
  remembers the context.
- `cargo fmt` keeps diffs minimal and reviewable. Unformatted commits
  fight against every future `git blame`.

## Build & Release

PeakBot ships as a single static binary for **Linux x86_64**, **Windows x86_64**, and **macOS universal2** (Intel + Apple Silicon in one fat binary). All three are produced from Linux via container builds — no native macOS/Windows host required. The driver is a top-level `Makefile`; cross-compilation is handled by three sibling Dockerfiles.

### Dockerfiles

| File | Builder image | Default `TARGET` | Notes |
|------|---------------|------------------|-------|
| `Dockerfile.linux` | `rust:1.88-bookworm` | `x86_64-unknown-linux-gnu` | Native build inside Debian; uses dummy-main caching trick |
| `Dockerfile.windows` | `rust:1.88-bookworm` + mingw | `x86_64-pc-windows-gnu` | Cross via gcc-mingw; final stage `FROM scratch` |
| `Dockerfile.macos` | `ghcr.io/rust-cross/cargo-zigbuild:latest` | `universal2-apple-darwin` | Cross via zig + bundled macOS SDK; produces fat binary (lipo merge) |

All three use a `FROM scratch` final stage and `--output type=local,dest=./output` so `make` extracts only the binary into `output/` without leaving an image in the local registry. Override the rust target with `--build-arg TARGET=...` if you need a single-arch macOS or a different libc.

> **Gotcha:** `ARG` does not propagate across `FROM` stage boundaries. Each Dockerfile redeclares `ARG TARGET=...` in its scratch stage so the `COPY --from=builder target/${TARGET}/release/...` path interpolates correctly.

### Make targets

Run `make help` for the full list. Day-to-day:

| Target | What it does |
|--------|--------------|
| `make` / `make build` | Build all three platforms in sequence (linux, windows, macos) |
| `make build-linux` | Build `output/peakbot-linux-amd64` (Linux x86_64) |
| `make build-windows` | Build `output/peakbot-windows-amd64.exe` (Windows x86_64) |
| `make build-macos` | Build `output/peakbot-macos-universal2` (macOS universal2) |
| `make clean` | `rm -rf output/` |
| `make rebuild` | `clean` + `build` (rebuilds all three) |
| `make help` | Print this table from `## ` doc comments in the Makefile |

Non-release builds produce **unversioned** filenames (e.g. `peakbot-linux-amd64`). The release flow injects the semver via `--build-arg VERSION=$v`, which the Dockerfiles splice into the artifact name as `peakbot-<v>-<platform>`. With `VERSION` unset (the default), the conditional `${VERSION:+-${VERSION}}` substitution in each Dockerfile collapses to nothing — same `cargo build --release`, just a cleaner name.

`CONTAINER_BUILDER` auto-detects `podman` (preferred) and falls back to `docker`. Override with `make build CONTAINER_BUILDER=docker`.

### Release pipeline

The `make release` target runs the full release flow end-to-end:

```
release-bump → release-tag → release-build-linux → release-build-windows → release-build-macos → release-publish
```

Each phase does one thing and can be re-run independently if a later phase fails. In-flight state is stashed in `.release-version` (gitignored) and deleted after a successful publish.

| Phase | Action |
|-------|--------|
| `release-bump` | Validate semver, refuse if tag exists, refuse on dirty tree (override with `ALLOW_DIRTY=1`), rewrite `[package].version` in `Cargo.toml`, sync `Cargo.lock` via `cargo update -p peakbot --precise <v>`, commit `chore: release <v>` |
| `release-tag` | Create annotated tag `<v>` (bare semver, no `v` prefix), push current branch + tag to `origin` |
| `release-build-{linux,windows,macos}` | Run the matching Docker build, copy artifact to `output/peakbot-<v>-{linux-amd64,windows-amd64.exe,macos-universal2}` |
| `release-publish` | Create a Gitea release via REST API and upload all three asset files |

#### Usage

```bash
export GITEA_TOKEN=...                       # required — generate at $GITEA_URL/user/settings/applications
make release                                 # interactive: prompts for version
make release VERSION=0.2.0                   # non-interactive
make release VERSION=0.2.0 ALLOW_DIRTY=1     # bypass clean-tree check
```

`GITEA_URL`, `OWNER`, and `REPO` are auto-derived from `git config --get remote.origin.url`, so a single `GITEA_TOKEN` is all you usually need. Override any of them on the CLI if your `origin` doesn't point at the release destination.

#### Required tools on the release host

- `git`, `cargo` — for the version bump and tag
- `podman` or `docker` — to run the three Dockerfile builds
- `curl`, `jq` — used by `release-publish` to talk to Gitea
- `awk` — portable invocation only (no PCRE lazy quantifiers); works under gawk / mawk / busybox awk

#### Resuming a partial failure

If `release-publish` dies mid-upload, `.release-version` still exists, the tag is already pushed, and the binaries are already in `output/`. Re-run just the failing phase:

```bash
make release-publish        # retry publish only
```

To start completely over, `git tag -d <v>`, `git push origin :refs/tags/<v>`, `git reset --hard HEAD~1`, `rm .release-version`, and re-run `make release`.


## Vision (image input)

PeakBot accepts image attachments on user turns. Images flow through the same
prompt path as text — tool calls fire normally on vision turns, and provider
compatibility is checked before the model is called.

### Syntax

Attach images inline with `[img:TOKEN]` tokens in your message:

```
what's in [img:~/pictures/cat.png]?
compare [img:/tmp/a.png] and [img:/tmp/b.png]
describe [img:https://example.com/photo.jpg]
```

Token resolution:
- Starts with `/`, `~`, or `./` → filesystem path (`~` expands to `$HOME`)
- Contains `://` → URL (OpenAI accepts; Anthropic refuses URLs)
- Anything else → rejected with `InvalidToken` error

Limits (`src/vision.rs`):
- Max 10 MB per image (checked via `fs::metadata` before reading)
- Max 8 images per turn
- Supported extensions: `.png`, `.jpg`/`.jpeg`, `.gif`, `.webp` (case-insensitive)

Failures surface as system messages in the chat log, never silently:
- `❌ file not found: /path/to/missing.png` — bad path
- `❌ file too large: ... (12 MB, max 10 MB)` — over cap
- `❌ Model `qwen/qwq-32b` does not support vision.` — wrong model

### Supported models

Detection is conservative (substring match on model name). Known-vision models:

- **OpenAI**: `gpt-4o`, `gpt-4-turbo`, `gpt-4.1`, `gpt-5`, `o1`, `o3`, `o4`
- **Anthropic**: `claude-3*`, `claude-opus*`, `claude-sonnet*`, `claude-haiku*`, `claude-4*`
- **Google**: `gemini-1.5*`, `gemini-2*`, `gemini-pro-vision`
- **Open models**: `pixtral*`, `llama-3.2-vision*`, `llava*`, `qwen2-vl*`, `qwen2.5-vl*`

Unknown model names default to `supports_vision = false` — attach an image
against an unrecognised model and PeakBot emits a system error instead of
shipping bytes that will be rejected downstream.

### Provider quirks

- **Anthropic** requires base64; URL attachments are refused (raise a provider
  error at the wire level). Use a filesystem path instead.
- **OpenAI** accepts both base64 and URLs.
- **Mistral** is known to panic on `UserContent::Image` in *assistant* messages
  during multi-turn sessions. Report an issue if you hit this; we can
  blocklist Mistral in `model_supports_vision` if it becomes a problem.

### Persistence

Images are stored **inline** as base64 in the conversation JSON (not as
sidecar files). A 5 MB PNG balloons the conversation JSON to ~7 MB. Acceptable
for v1; sidecar storage is a speculative Phase 2 (see `one-vision.md`).

### Internals

- Entry: `src/vision.rs::parse_attachments_inline` (buffer → text + attachments)
- Wire conversion: `src/state/state_manager.rs::user_content_from_attachment`
- Capability flag: `ProviderInfo::supports_vision` set by
  `vision::model_supports_vision(&model)` in every provider constructor
- Dispatch path: `SubmitKind::MultimodalMessage` → `add_user_message_with_attachments`
  → `build_current_turn_message` → `prompt_with_history` (identical path to text turns)
