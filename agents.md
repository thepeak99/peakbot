# PeakBot Documentation

## Overview

PeakBot is a single-agent coding assistant built with [Rig](https://github.com/0xPlaygrounds/rig) (`rig-core` v0.31). It runs as a terminal REPL, equipped with filesystem, shell, web fetch, and web search tools. It also supports dynamically loading tools from MCP (Model Context Protocol) servers and Agent Skills.

## Architecture

```
User (stdin)
  │
  ▼
┌──────────────────────────────────────────────┐
│  REPL Loop  (src/main.rs)                    │
│  Reads input, passes to agent, prints output │
│  Maintains chat_history: Vec<Message>        │
│  Commands: /stats, /context, /compact        │
└──────────────┬───────────────────────────────┘
               │
               ▼
┌──────────────────────────────────────────────┐
│  Provider Abstraction (src/providers/)       │
│  ┌──────────────────┐ ┌──────────────────┐   │
│  │ OpenRouter       │ │ Ollama           │   │
│  │ (with cost hook) │ │ (local models)   │   │
│  └──────────────────┘ └──────────────────┘   │
│  CostTracker: Token cost tracking for API    │
└──────────────┬───────────────────────────────┘
               │
               ▼
┌──────────────────────────────────────────────┐
│  Context Manager (src/context_manager.rs)    │
│  Auto-compacts context when threshold reached│
│  Uses actual token counts from provider      │
└──────────────┬───────────────────────────────┘
               │
               ▼
┌──────────────────────────────────────────────┐
│  Rig Agent                                   │
│  System prompt: built from + skills + env    │
│  Max tokens: configurable                    │
│  Max tool turns per message: configurable    │
└──────────────┬───────────────────────────────┘
               │
               ▼
┌──────────────────────────────────────────────┐
│  Tool Set                                    │
│                                              │
│  ┌─────────────┐  ┌─────────────┐            │
│  │ file_edit   │  │ file_read   │            │
│  │ 4 commands  │  │ line ranges │            │
│  └─────────────┘  └─────────────┘            │
│  ┌─────────────┐  ┌────────────────┐         │
│  │ bash        │  │ list_directory │         │
│  │ /bin/sh -c  │  │ recursive opt  │         │
│  └─────────────┘  └────────────────┘         │
│  ┌─────────────┐  ┌────────────────┐  ┌────┐ │
│  │ fetch_url   │  │ web_search     │  │thnk│ │
│  │ HTTP GET    │  │ (SearXNG)      │  │    │ │
│  └─────────────┘  └────────────────┘  └────┘ │
│  ┌─────────────────────────────────────────┐ │
│  │ MCP tools (dynamic)                     │ │
│  └─────────────────────────────────────────┘ │
└──────────────────────────────────────────────┘
```

## Core Components

### Provider Abstraction (`src/providers/mod.rs`)

PeakBot supports multiple LLM providers through a unified provider abstraction:

| Provider | Features | Cost Tracking |
|----------|----------|---------------|
| **OpenRouter** | Access to 100+ models via API | ✅ Full support |
| **Ollama** | Local models (llama3, qwen, mistral, etc.) | ❌ Not supported |

The provider system provides:
- `DynAgent` - Dynamic enum for runtime provider switching
- `CostTracker` - Unified interface for session statistics
- `ProviderInfo` - Metadata (name, model, pricing support)

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

Extends the agent with post-processing capabilities:

- **TokenCostHook** (`src/hooks/token_cost.rs`): Tracks token usage and calculates API costs for OpenRouter
- **SessionStats**: Accumulates total tokens, requests, and estimated cost
- **ModelPricing**: Per-model pricing data (input/output per 1M tokens)

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
) -> Result<(DynAgent, ProviderInfo, CostTracker)>
```

The agent uses:
- The configured LLM provider (OpenRouter or Ollama)
- Built-in tools (7 tools + optional search)
- Any MCP tools from configuration
- Skills loaded from the skills directories

The agent is stateless between tool turns -- all state lives in `chat_history` (conversation memory) and `FileEditTool.file_history` (undo stack). Rig owns the agentic loop: it automatically dispatches tool calls, collects results, and loops until the model produces a text response or hits the 50-turn limit.

## Tools

All tools live in `src/tools/` and implement `rig::tool::Tool`. Each tool defines:
- `NAME` -- unique string identifier sent to the model
- `Args` -- struct deserialized from JSON the model produces
- `Output` -- type serialized back to the model as a tool result
- `Error` -- tool-specific error type (errors are sent back to the model, not fatal)
- `definition()` -- returns the JSON Schema the model uses to know what to send
- `call()` -- executes the tool logic

PeakBot includes **7 built-in tools** (6 always available, 1 conditional):

### file_edit (`src/tools/file_edit.rs`)

The primary editing tool, modeled after Anthropic's `text_editor_20250728`. A single tool with a `command` discriminator selecting between four operations.

| Command | Required Params | Behavior |
|---------|----------------|----------|
| `view` | `path` | Read file with line numbers. Optional `view_range: [start, end]` (1-indexed, -1 = EOF). On directories, lists contents. |
| `create` | `path`, `file_text` | Write a new file. Fails if file already exists. Creates parent dirs if needed. |
| `str_replace` | `path`, `old_str` | Find `old_str` (must be unique in file), replace with `new_str` (omit to delete). Returns context snippet around edit. |
| `insert` | `path`, `insert_line`, `insert_text` | Insert text after line N (0 = beginning). Returns context snippet. |

Design decisions:
- **Uniqueness enforcement** on `str_replace`: if `old_str` matches 0 or >1 times, the tool returns an error with line numbers to help the model refine.
- **Undo history**: `file_history: Mutex<HashMap<PathBuf, Vec<String>>>` stores previous file contents on each edit. Not exposed as a command yet but the stack is there.
- **Truncation**: outputs > 10,000 chars are clipped with a notice suggesting `grep -n` or line ranges.
- All paths must be absolute.

### file_read (`src/tools/file_read.rs`)

Simple read-only tool. Takes `path` with optional `start_line`/`end_line` (1-indexed, inclusive). Returns content with `cat -n` style line numbering. Truncates at 10,000 chars.

Exists separately from `file_edit view` to give the model a simpler, single-purpose tool for the common "just read this file" case.

### bash (`src/tools/bash.rs`)

Runs shell commands via `/bin/sh -c`. Returns exit code, stdout, and stderr.

| Param | Default | Range |
|-------|---------|-------|
| `command` | required | any string |
| `timeout_seconds` | 30 | 1-120 |

Implementation details:
- Uses `tokio::process::Command` with `.kill_on_drop(true)` so timed-out processes are cleaned up automatically.
- Timeout is enforced with `tokio::time::timeout` wrapping `wait_with_output()`.
- stdout/stderr each truncated independently at 10,000 chars.

### list_directory (`src/tools/list_directory.rs`)

Lists directory contents with optional recursion.

| Param | Default | Notes |
|-------|---------|-------|
| `path` | required | absolute path to directory |
| `recursive` | false | if true, recurse up to depth 3 |

Entries are sorted alphabetically. Directories get a trailing `/`. Hidden files (`.` prefix) are skipped.

### fetch_url (`src/tools/fetch_url.rs`)

Fetches the content of a URL via HTTP GET request.

| Param | Default | Notes |
|-------|---------|-------|
| `url` | required | any valid HTTP/HTTPS URL |

Implementation details:
- Uses `reqwest` for HTTP requests with a 30-second timeout.
- Returns the HTTP status code, reason phrase, and response body.
- Response body is truncated at 50,000 characters.
- Sets `User-Agent: PeakBot/1.0` header.

### web_search (`src/tools/search.rs`)

Web search tool using SearXNG instances. Requires SearXNG to be configured in `config.yaml`.

| Param | Default | Notes |
|-------|---------|-------|
| `query` | required | The search query |
| `category` | optional | "images", "videos", "news", "maps", "music", "science" |
| `site` | optional | Filter to specific site (e.g., "github.com") |
| `num_results` | 10 | Max results (1-20) |
| `time_range` | optional | "day", "month", "year" |

Implementation details:
- Connects to a SearXNG instance configured via `searxng.base_url`
- Uses JSON format for API responses
- Returns title, URL, and snippet for each result
- Requires JSON format to be enabled on the SearXNG instance
- Configurable timeout (default: 30s) and max results (default: 10)

### think (`src/tools/think.rs`)

Reasoning tool for complex thinking and brainstorming. Allows the model to pause and think through problems before taking action.

| Param | Default | Notes |
|-------|---------|-------|
| `thought` | required | The thought process to execute |

Implementation details:
- Useful for multi-step reasoning, bug analysis, and planning
- Logs thoughts to tracing for debugging
- Returns a confirmation with the thought content

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
7. When the model produces a text response, TokenCostHook tracks token usage
8. Results are printed and session stats are updated

## Error Handling

- **Tool errors** (bad path, non-unique match, timeout) are returned to the model as tool results. The model sees the error message and self-corrects. These do not crash the agent.
- **API errors** (auth failure, network) surface as `PromptError` and are printed in the REPL. The loop continues.
- **Max turns exceeded** means the model used all 50 tool rounds without producing a final answer. Printed as an error; the user can retry or rephrase.

## Configuration

PeakBot supports multiple LLM providers (OpenRouter, Ollama). Configuration is loaded from `config.yaml` in the platform config directory, with environment variables taking precedence.

### Provider Configuration

The recommended config format uses the `provider` key:

```yaml
# OpenRouter example
provider:
  type: openrouter
  config:
    api_key: sk-or-v1-xxx
    model: anthropic/claude-3.7-sonnet
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
| `OLLAMA_MODEL` | Ollama model name (legacy) |
| `OLLAMA_BASE_URL` | Ollama base URL (legacy) |
| `OLLAMA_TEMPERATURE` | Ollama temperature (legacy) |
| `OLLAMA_NUM_CTX` | Ollama context size (legacy) |
| `AGENT_MAX_TURNS` | Max tool turns per message |
| `MCP_SERVERS` | JSON array of MCP server configs |

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
├── lib.rs                  # AgentRunner, system prompt building
├── config.rs               # Configuration loading from config.yaml + env vars
├── context_manager.rs      # Context compaction for long conversations
├── system_prompt.txt       # Base system prompt (included at compile time)
├── providers/
│   └── mod.rs              # Provider abstraction (OpenRouter, Ollama), DynAgent
├── hooks/
│   ├── mod.rs              # Hook exports
│   └── token_cost.rs       # TokenCostHook for API cost tracking
├── skills/
│   ├── mod.rs              # Skills module exports
│   ├── discovery.rs        # Skill discovery and SkillRegistry
│   ├── parser.rs           # SKILL.md parsing
│   └── types.rs            # Skill data structures
└── tools/
    ├── mod.rs              # Re-exports: all built-in tools
    ├── bash.rs             # BashTool -- shell execution with timeout
    ├── fetch_url.rs        # FetchUrlTool -- HTTP GET requests
    ├── file_edit.rs        # FileEditTool -- view/create/str_replace/insert
    ├── file_read.rs        # FileReadTool -- read with line ranges
    ├── list_directory.rs   # ListDirectoryTool -- dir listing with recursion
    ├── search.rs           # SearchTool -- SearXNG web search
    ├── think.rs            # ThinkTool -- reasoning tool
    └── logging_wrapper.rs  # LoggingToolDyn -- wrapper for MCP tool tracing
```

