# PeakBot Documentation

## Overview

PeakBot is a single-agent coding assistant built with [Rig](https://github.com/0xPlaygrounds/rig) (`rig-core` v0.38). It runs as a terminal REPL, equipped with filesystem, shell, web fetch, and web search tools. It also supports dynamically loading tools from MCP (Model Context Protocol) servers and Agent Skills. Additional features include conversation persistence, todo list management, and an event-driven hooks system for cost tracking.

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
| **Anthropic** | Claude API or any Anthropic-compatible server (e.g. llama.cpp `/v1/messages`); configurable `base_url`. The only provider whose tool results carry images — enables `view_image` | ❌ Not supported | Messages API |
| **LlamaCpp** | llama.cpp compatible endpoints | ✅ Supported | Completion API |
| **Ollama** | Local models (llama3, qwen, mistral, etc.) | ❌ Not supported | Completion API |

The provider system provides:
- `DynAgent` - Dynamic enum for runtime provider switching
- `CostTracker` - Unified interface for session statistics
- `ProviderInfo` - Metadata (name, model, pricing support)

**DynAgent Variants**:
- `OpenRouter(Agent<CompletionModel, SessionHook>)` - Full cost tracking
- `OpenAI(Agent<ResponsesCompletionModel, SessionHook>)` - Uses OpenAI's responses API
- `Anthropic(Agent<CompletionModel, SessionHook>)` - Messages API; carries images in tool results (`view_image`)
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

### Background Processes (`src/bg_processes.rs`)

Long-running PTY-backed processes spawned by the [`bash_bg`](#tools-overview) tool. Each process gets a numeric id, a ring buffer of captured output lines, and a per-process `cooldown` that paces how often its output is injected as a synthetic turn.

**Data flow** (the "two seams" from `bash-background.md`):

1. `bash_bg start` spawns a PTY via `portable-pty`, launches the child under `sh -c`, and registers the process with `StateManager::bg`.
2. A per-process reader thread streams output lines into the ring buffer (ANSI-stripped, debounced at 500ms) and pings the agent loop via a tokio mpsc channel.
3. Between turns (and on a deadline-driven wakeup), the agent loop calls `StateManager::drain_bg_output_into_synthetic_turn`, which builds a `[bg output]` block per *eligible* buffer and runs an agent turn over it. The synthetic message is persisted with `MessageSource::Background { proc_ids }` so the renderer styles it (🛰 Background) on `/load`.

**Cooldown** (per-process, `cooldown_secs`, default 60):

- After a process injects output, further output is **coalesced** — accumulated in the ring buffer and flushed in one batch once the process's cooldown elapses. `drain_outputs(now)` gates each dirty process on `last_inject + cooldown <= now` (a pure function of an injected `now`, so the decision is unit-testable).
- `cooldown_secs: 0` ⇒ real-time (every drained batch injects). Used for external-input bridges (telegram, webhooks) so each line surfaces immediately.
- **Process exits always bypass the cooldown** — a one-shot terminal transition the model must learn at once.
- A quiet buffer that fell silent mid-window still flushes on time: the agent-loop forwarder arms a wakeup at `BgRegistry::next_poke_deadline` (the soonest pending window expiry).
- A real user message calls `reset_bg_cooldowns`, clearing every process's timer so buffered output flushes alongside the user's turn.

**Lifecycle**:

- `/new`, `/model`, `/load` kill all bg processes and clear the registry.
- App exit drops the registry (`Drop` impl sends SIGHUP to every child and joins reader threads).
- Buffers and live processes do **not** persist across restarts — the registry is in-memory only.

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

PeakBot includes **12 always-on built-in tools** (plus 2 opt-in vector
tools and 1 provider-gated `view_image` tool, see below):

### Tools Overview

PeakBot includes **12 always-on built-in tools**, plus **2 opt-in vector
tools** (`doc_index` / `doc_search`) registered only when a `vector_db:`
block is configured, plus **1 provider-gated tool** (`view_image`)
registered only on the Anthropic provider — **15 total** when both the
vector store and the Anthropic provider are active:

| Tool | File | Description |
|------|------|-------------|
| `file_create` | `file_edit/create.rs` | Create a new file (refuses to overwrite existing) |
| `file_str_replace` | `file_edit/str_replace.rs` | Replace exact text in an existing file, with whitespace-flexible fallback matching |
| `file_insert` | `file_edit/insert.rs` | Insert text at a specific line in an existing file |
| `file_read` | `file_read.rs` | Read files with line ranges |
| `pdf_read` | `pdf_read.rs` | Extract text or Markdown from a PDF (all pages, or a 1-indexed inclusive `start_page`/`end_page` range; `format` = `text`/`markdown`). Built on `pdf_oxide` (pure-Rust, cross-target-safe). Truncates to 50k chars. |
| `list_directory` | `list_directory.rs` | List directory contents with recursion |
| `bash` | `bash.rs` | Execute shell commands with timeout, truncate to last 50k chars, save full output to temp |
| `bash_bg` | `bash_bg.rs` | Spawn long-running PTY-backed processes (4 verbs: start/stop/list/send_line). The model is notified automatically — a synthetic `[bg output]` user turn lands when a process's output is flushed and on every process exit (including `capture_output_lines: 0`). Never poll. Per-process `cooldown_secs` (default 60) coalesces output; `0` = real-time for external-input bridges (telegram/webhooks) — see `bash-background.md`. |
| `fetch_url` | `fetch_url.rs` | HTTP GET requests to URLs |
| `web_search` | `search.rs` | SearXNG-based web search |
| `think` | `think.rs` | Reasoning tool for complex thinking |
| `todo` | `todo.rs` | Todo list management |
| `doc_index` *(opt-in)* | `doc_index.rs` | Parse → chunk → embed → store a file/dir into the semantic vector store. Idempotent re-index (skips unchanged files via stored `sha256`). Registered only when `vector_db:` is configured. |
| `doc_search` *(opt-in)* | `doc_search.rs` | Embed a query and return the top-k most relevant indexed chunks with source + score. Registered only when `vector_db:` is configured. |
| `view_image` *(Anthropic-only)* | `view_image.rs` | Load a local image file (PNG/JPEG/GIF/WEBP, ≤10 MB) into the model's vision context as a structured image tool result. Reuses `vision::load_image_from_path`. Registered **only on the Anthropic provider** — the lone rig provider whose tool-result channel delivers images (OpenRouter substitutes a placeholder, Ollama drops it, OpenAI errors). |
| `delegate` *(opt-in)* | `pipeline/delegate_tool.rs` | `delegate(role, task) -> string`: run one sub-agent to completion on a fresh context and return its final text. Registered **only when `pipeline.enabled: true` AND the conversation opted in** (per-conversation, default off — see [Multi-agent pipeline](#multi-agent-pipeline-orchestrator--sub-agents)). Sequential by construction (no parallel mode). |

Plus optional **MCP tools** from configured servers.

### The `thought` field (ThoughtGate)

No tool declares a `thought` parameter of its own. Instead, every built-in
**and** MCP tool is wrapped at registration in `ThoughtGate`
(`src/tools/thought_gate.rs`), a `ToolDyn` decorator that owns the
`thought` concern centrally:

1. **Schema injection** — `definition()` adds a required `thought` string
   property to the inner tool's schema (idempotent: skipped if the inner
   tool already exposes `thought`). The model is always *told* `thought`
   is required.
2. **Strip before delegate** — `thought` is synthetic; the inner tool / MCP
   server never declared it, so the gate removes it from the args before
   calling (a strict MCP server may reject unknown params).
3. **Soft nudge, never a hard error** — because the gate parses args as
   `serde_json::Value`, a missing/blank `thought` can never produce a
   `JsonError` that skips the call. The tool runs regardless; when
   `thought` was absent, null, or whitespace, a one-line reminder is
   appended to the result.

Two tools are **exceptions** — registered ungated:

- `think` — its `thought` *is* the payload it echoes back, not metadata.
  Wrapping it would strip the very thing it returns.
- `todo` — the task text already carries the plan, so `thought` is
  redundant; and some models (e.g. MiniMax) structurally refuse it,
  tripping the gate's nudge on every todo call.

The two vector tools share a single `VectorStore` (`src/vector/mod.rs`)
built once at startup and injected into both — the same injection pattern
as `SearchTool::new(config)`. The store wraps a `ruvector-core` `VectorDB`
(HNSW + redb) behind `Arc` (single redb writer) plus an `EmbeddingsClient`
(`src/vector/embeddings.rs`) that hits any OpenAI-compatible
`/v1/embeddings` endpoint. Sync DB ops run under `spawn_blocking`. The
`VectorDB` itself is created **lazily on the first write** (held in a
`tokio::sync::OnceCell` inside an `Arc<StoreInner>` shared by both tool
clones): `VectorStore::open` touches no disk, and reads (`search`,
re-index skip-check) before any index are pure no-ops — so enabling
`vector_db` does not write `.peakbot/vectors.db` until the first chunk is
indexed. See the `vector_db:` config block under
[Configuration](#configuration).


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

### Live reload on session verbs (`/new`, `/model`, `/cd`, `/load`)

Editing `config.yaml` (master + per-repo `.peakbot/config.yaml`) or skills
under `.agents/skills` no longer requires a restart. Each of the four session
verbs re-reads config and re-scans skills for the *running session* before it
rebuilds the agent — reload is per-session (it never mutates the process-wide
`SessionDeps` shared across web tabs).

Failures are handled at the boundary and never crash the session: malformed
YAML or an invalid `default_model` warns (`⚠ config reload failed … — keeping
previous config.`) and keeps the previously-loaded config running.

`/new` reloads and rebuilds on your **currently-active** model — it refreshes
skills, the system prompt, and ancillary config, but does *not* bounce you to
`default_model` (that would surprise anyone who'd just `/model`-switched). To
adopt a changed `default_model`, `/model <alias>` it, or restart.

| Reload-safe (takes effect on the next session verb) | Boot-only (edit → restart; diffed and flagged with `⚠ … ignored — restart to apply.`) |
|-----------------------------------------------------|----------------------------------------------------------------------------------------|
| `providers:` / `default_model` (rebuilt registry — new aliases resolve) | `mcp_servers` (live subprocesses)          |
| skills + system prompt (re-scanned)                 | `vector_db` (redb/HNSW handle)             |
| `searxng.*`, `bash.env`, `agent_max_turns`          | `web.*` (read once by the session reaper)  |
| `cost_tracking`, `context.*`, `retry.*`, `memory.*` | `pipeline` (a built sub-agent registry)    |
| `tools.*` (built-in filter — re-applied on agent rebuild) | `provider` (legacy block — owned by the resolve step, never overwritten by reload) |

Not reloaded (rejected by design): a filesystem watcher / auto-reload on save —
reload is explicit via the session verbs to avoid heisenbugs mid-turn.

### Multi-model with `/model`

The recommended shape declares **a list of providers, each with its own list of models**, plus `default_model` to pick the boot alias. Use `/model` to list, `/model <alias>` to switch.

```yaml
providers:
  - name: openrouter
    type: openrouter
    api_key: sk-or-v1-xxx
    models:
      - name: anthropic/claude-3.7-sonnet
        alias: sonnet
        max_tokens: 8192
      - name: anthropic/claude-opus-4
        alias: opus
      - name: google/gemini-2.0-flash-001
        alias: flash

  - name: openai
    type: openai
    api_key: sk-xxx
    models:
      - name: gpt-4o
        alias: oai-gpt4
        max_tokens: 4000
      - name: o3                      # no alias → user types `/model o3`

  - name: local
    type: ollama
    base_url: http://localhost:11434
    models:
      - name: qwen2.5-coder:14b
        alias: local
        temperature: 0.4

default_model: sonnet
```

Rules:
- `alias` is optional. **When omitted, the model is addressable only as `<provider_name>/<model_name>` — the full qualified handle.** It is *never* addressable by the bare model leaf alone. Aliases are globally unique and must match `^[A-Za-z0-9_./:-]+$`.
- The literal alias `unknown` is reserved and rejected at config load (used as the sentinel for pre-v4 conversation files).
- `default_model` is required iff any models are declared, and must reference one of the declared aliases.
- `provider name` is informational — it's only used in `/conversations` and the `/model` listing, never cross-referenced.
- Per-model overrides: `max_tokens`, `temperature`, `num_ctx` (Ollama), `extra_params` (LlamaCpp), `context_window_override`, `vision` (`true`/`false` to force image support on/off; omit for auto-detection — see [Vision](#vision-image-input)).

`/model` semantics:
- `/model` (no arg) — lists every alias with the active one marked `→`.
- `/model <alias>` — **starts a new conversation** on that model. Confirms with the same Ctrl+C-style overlay if the current chat has content; skips the prompt on an empty conversation.
- `/model <unknown>` — emits `❌ /model: unknown alias \`x\`. Available: …`. No state change.
- `/model <current>` — emits `Already on x.`. No destructive reset.
- The active model alias is **bound to the conversation** in metadata. `/load <id>` re-activates the saved alias; if the alias is no longer in the registry (renamed/removed in config, or pre-v4 file with no alias), the load is rejected with `Model 'xyz' not available.` and the previous conversation stays intact. *(persisted artifacts must carry every field needed to be re-activated.)*
- MCP servers persist across switches — only the agent handle is rebuilt; MCP subprocesses keep running.

### Legacy single-provider config (still supported)

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

# Anthropic example (Claude API, or any Anthropic-compatible server).
# This is the provider that supports `view_image` — point base_url at a
# local llama-server's Messages endpoint to feed images to a local
# multimodal model. Omit base_url to use the hosted Claude API.
provider:
  type: anthropic
  config:
    api_key: optional        # Optional for local servers; required for Claude
    base_url: http://localhost:8080   # llama-server /v1/messages; default: https://api.anthropic.com
    model: your-multimodal-model      # e.g. claude-3-5-sonnet-latest, or a local GGUF name
    max_tokens: 4096
    # Prompt caching (default: auto). Injects ephemeral `cache_control`
    # breakpoints to cut input-token cost on the stable request prefix.
    #   auto    — top-level breakpoint the API advances as the chat grows, 5m
    #   auto_1h — same as auto, 1-hour TTL
    #   manual  — system prompt + last tool + last message (≈ LiteLLM
    #             system/0 + user/-1 injection points), 5-minute TTL
    #   off     — no caching (set explicitly for local llama-server endpoints
    #             that may not honor `cache_control`)
    # In the multi-model `providers:` format, set `prompt_caching:` per model.
    # prompt_caching: off
    # Force image support on (registers `view_image` + allows `[img:…]`) for a
    # model whose name auto-detection doesn't recognise as vision-capable —
    # e.g. a local multimodal GGUF. Omit for auto-detection. `vision: false`
    # forces it off. In the multi-model format, set `vision:` per model.
    # vision: true

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
  bearer_token: "optional-token"   # optional: sent as `Authorization: Bearer …`

# Vector DB — semantic document memory (doc_index / doc_search tools).
# When this block is absent or `enabled: false`, neither tool is registered.
vector_db:
  enabled: true
  db_path: ./.peakbot/vectors.db              # per-repo; default if omitted
  embeddings:
    base_url: https://api.openai.com/v1        # any OpenAI-compatible endpoint
    api_key: sk-...                            # optional for local servers
    model: text-embedding-3-small
    dimensions: 1536                           # must match the model's output
# Notes:
#   - The embeddings endpoint is configured independently of the chat
#     provider — point it at OpenAI, llama.cpp, Ollama, LM Studio, TEI, etc.
#   - `dimensions` must match the model AND any existing DB at `db_path`.
#     On reopen, ruvector uses the STORED dimensions/metric; a model whose
#     dims differ from the existing DB is rejected with an actionable error.
#   - Chunk size (1000 chars), overlap (200), distance (Cosine), and default
#     `k` (5) are constants, not config.
#   - `.peakbot/vectors.db` is gitignored.

# Context compaction settings
context:
  enabled: true
  threshold: 0.8          # Trigger at 80% context usage
  keep_recent: 5          # Always keep last 5 messages
  context_window: 200000  # Or 0 for auto-detect

# Token cost tracking (OpenRouter only)
cost_tracking: true

# Memory.md feature. Governs BOTH the memory.md instructions injected into the
# system prompt AND the auto-compaction of an oversized memory.md at
# conversation start. `enabled: false` turns off the whole feature — the agent
# is never told to read/update memory.md and no compaction runs. (Default on.)
memory:
  enabled: true
  threshold_bytes: 51200   # compact memory.md once it exceeds this (default 50 KB)

# Built-in tool filter. Pick ONE of `disabled` (blocklist: these are removed,
# the rest stay) or `only` (allowlist: only these stay, the rest are removed).
# Setting both is a config error. Names are the tool wire names (see the Tools
# table above); an unknown name is rejected at load. Absent block = every tool
# available. Reload-safe (applied on the next session verb).
tools:
  disabled: [bash_bg, web_search]   # blocklist — XOR with `only`
  # only: [file_read, file_str_replace, bash]

# MCP servers
mcp_servers:
  - name: filesystem
    command: "npx"
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]

  # Streamable-HTTP server with a static bearer token.
  - name: my-bearer-mcp
    type: streamable-http
    url: https://example.com/mcp
    auth:
      type: bearer
      token: "xxx-keep-me-secret"

  # OAuth 2.1 + DCR + PKCE (e.g. Linear).
  # On first connect, PeakBot opens your browser to authorise. The token
  # is cached under `~/.cache/peakbot/mcp-auth/<name>.json` (mode 0600 on
  # Unix). Subsequent runs skip the browser. Refresh is silent.
  - name: linear
    type: streamable-http
    url: https://mcp.linear.app/mcp
    auth:
      type: oauth

  # OAuth 2.1 + static client credentials + PKCE (e.g. Google Workspace).
  # Google's Gmail/Drive/Calendar MCP servers do NOT support Dynamic
  # Client Registration — you must create an OAuth 2.0 Desktop-app
  # client in Google Cloud Console, copy the client_id + client_secret,
  # and list the exact scopes you configured the consent screen for.
  # Loopback redirects on any port are accepted (RFC 8252 §7.3), so no
  # extra coordination with the console is needed.
  - name: gmail
    type: streamable-http
    url: https://gmailmcp.googleapis.com/mcp/v1
    auth:
      type: oauth
      client_id: "1234567890-xxxxxx.apps.googleusercontent.com"
      client_secret: "GOCSPX-xxxxxxxxxxxx"
      scopes:
        - https://www.googleapis.com/auth/gmail.readonly
        - https://www.googleapis.com/auth/gmail.compose
```

> **OAuth on SSH.** When `$SSH_CONNECTION` is set PeakBot prints the
> authorisation URL instead of launching a browser (which would open on
> the wrong machine). To complete the flow from a remote host, forward
> the ephemeral callback port back to your workstation:
> `ssh -L 9999:127.0.0.1:9999 user@host` — the URL printed by PeakBot
> tells you the actual port to use.

> **Deprecated:** the top-level `auth_token: "xxx"` field still works
> but emits a warning on connect. Migrate to `auth: { type: bearer,
> token: "xxx" }`. Setting both is a hard config error.

### Environment Variables

| Environment Variable | Description |
|---------------------|-------------|
| `PROVIDER` | JSON provider config (new format) |
| `AGENT_MAX_TURNS` | Max tool turns per message |
| `MCP_SERVERS` | JSON array of MCP server configs |
| `SEARXNG_BASE_URL` | SearXNG base URL |
| `PEAKBOT_WEB_TOKEN` | Shared secret guarding `peakbot --web` (fallback for `--token`). Required when `--bind` targets a non-loopback address. |

### REPL Commands

| Command | Description |
|---------|-------------|
| `/stats` | Show session statistics (tokens, cost) |
| `/context` | Show context usage status |
| `/compact` | Force context compaction |
| `/bg` | List background processes (`bash_bg` registry) |
| `/subagents on\|off` | Enable/disable sub-agents for the current conversation (only before the first turn; requires a configured `pipeline:`) |
| `exit` | Quit the REPL |

### Settings

| Setting | Default | Description |
|---------|---------|-------------|
| Model | `anthropic/claude-3.7-sonnet` | LLM model to use |
| Max tokens | 4096 | Maximum tokens in response |
| Max tool turns | 50 | Tool calls per prompt |
| Output truncation | 10,000 chars | File tool output limit |
| Bash timeout | 30s | Shell command timeout |

### Web UI (`peakbot --web`)

`peakbot --web` serves the embedded SPA + live chat over WebSocket. CLI flags:

| Flag | Default | Description |
|------|---------|-------------|
| `--web` | off | Run the web UI instead of the terminal UI. Binds loopback. On a local, non-SSH session it auto-opens your browser. |
| `--bind <host:port>` | `127.0.0.1:7823` | Listen address. A non-loopback bind **requires** a token, else PeakBot refuses to start. |
| `--token <secret>` | — | Shared secret guarding every route (assets, `/commands`, `/ws`). Falls back to `PEAKBOT_WEB_TOKEN` (preferred — keeps the secret out of shell history). |
| `--tls` | off | Serve over HTTPS with PeakBot's built-in CA (batteries included). Overrides `web.tls`. See [HTTPS](#https-with-a-built-in-ca-tls) below. |
| `--tls-name <NAME>` | — | Add an extra DNS name or IP to the HTTPS certificate. Repeatable. Augments the auto SANs (loopback, LAN IP, `<hostname>.local`) — never replaces them. Requires `--tls`. |

**Sticky sessions (issue #118).** A conversation is addressed in the URL as
`?convo=<uuid>`, and a live agent session (StateManager + controller loop)
stays bound to that conversation independently of any socket. A dropped
WebSocket (refresh, network blip, server restart) re-attaches to the **same**
running session on reconnect — transcript, todos, and stats survive. Multiple
tabs on the same `?convo=` URL share one session and see the same state. A
session is "active" while a socket-independent live session exists for it
(registry membership, never persisted); the conversations picker marks active
rows and can kill one for everyone attached. A session is reaped only once it
is **fully idle** — no sockets attached, the agent not processing a turn, *and*
no live `bash_bg` children under it — for `web.session_ttl_secs` (#158). Any of
those three keeps it live and resets the idle clock, so closing the tab
mid-turn (or while a long `bash_bg` runs) never reaps the running agent; the
clock only starts once all three go quiet. The reaper samples the live
agent/bg signals on each tick — it is not told when a turn starts or ends,
keeping the registry web-only. The terminal (`--stdio`, REPL) Views remain
single-session. See `src/ui/web/registry.rs`.

```yaml
web:
  session_ttl_secs: 600    # reap a fully-idle session (no sockets, agent idle,
                           # no live bash_bg) after 10 min (default)
  reaper_tick_secs: 60     # how often the reaper scans (default)
  tls: false               # serve HTTPS with the built-in CA (default false)
```

### HTTPS with a built-in CA (`--tls`)

`peakbot --web --tls` (or `web.tls: true`) serves the UI over HTTPS with PeakBot
owning the whole PKI — batteries included, no OpenSSL, no manual certs.

**How it works.** On first use PeakBot self-signs a long-lived **CA** and stores
it in `dirs::cache_dir()/peakbot/tls/` (`ca-cert.pem` + `ca-key.pem`, the key
`0600` — same home and guarantee as the mcp-auth credential store). Every boot it
mints a fresh **leaf** signed by that CA, whose SANs follow the machine's current
addresses: `localhost`, `127.0.0.1`, `::1`, the primary LAN IP, any concrete bind
IP, and the machine's mDNS **`<hostname>.local`** name (derived from the system
hostname's first label). The `.local` name is the most durable handle — it
survives DHCP lease changes that shuffle the LAN IP, so a phone bookmarked at
`https://mymachine.local:7823` keeps working. Add further names with
**`--tls-name <NAME>`** (repeatable; DNS or IP, classified automatically) — these
augment the auto SANs, never replace them. You install the CA on your phone
**once**; every leaf it signs is then trusted, so a changing LAN IP needs no phone
action. The CA is generated once and
**never silently regenerated** (that would break every device that trusts it) — to
rotate, delete the `tls/` directory. Implementation: `src/ui/web/tls.rs`
(`rcgen` + `rustls`/`aws-lc-rs`, both routed through the provider `reqwest`
already links; `rcgen`'s heavy `x509-parser` feature is deliberately avoided —
the CA identity is fixed, so the issuer is rebuilt from the persisted key alone).

**Installing the CA on a phone.** PeakBot serves the CA public certificate at the
**tokenless** route `GET /peakbot-ca.crt` (MIME `application/x-x509-ca-cert`, so
phones offer to install it directly). This route is intentionally exempt from the
token gate — a CA *public* cert carries no secret; only its private key (never
served) can sign. On boot PeakBot prints the CA-install URL using the LAN IP:

```
🔒 PeakBot web UI (HTTPS): https://127.0.0.1:7823/  (Ctrl+C to quit)
📲 Install the CA on your phone once: https://192.168.1.42:7823/peakbot-ca.crt
   iOS: after installing, enable it under Settings → General → About → Certificate Trust Settings.
   CA stored at /home/you/.cache/peakbot/tls
```

> **Mobile trust caveats.** Browsers (Chrome/Safari) honour user-installed CAs, so
> browsing to the PeakBot UI works. (Android 7+'s "apps ignore user CAs by default"
> restriction is for *native apps*, not browsers.) On **iOS** installing the
> profile is a two-step: install it, then enable it under *Settings → General →
> About → Certificate Trust Settings*.

TLS **complements** the token, it does not replace it: a non-loopback bind still
requires a token. BYO-cert config and a QR-on-boot are deliberate non-goals for
now (YAGNI).

```bash
# Local HTTPS (loopback)
peakbot --web --tls

# Add extra names to the cert (e.g. a router-assigned hostname + a static IP)
peakbot --web --tls --tls-name peakbot.lan --tls-name 10.0.0.9

# Remote HTTPS (token still required for non-loopback)
PEAKBOT_WEB_TOKEN=$(openssl rand -hex 16) peakbot --web --tls --bind 0.0.0.0:7823
```


**Auth model.** When a token is set, the browser presents it once as
`?token=…`; the server sets a `peakbot_token` cookie and all later requests —
including the same-origin `/ws` upgrade — authenticate via that cookie (no
frontend token-threading). The token in the URL lands in browser history, so
it is designed to appear there only once (the cookie takes over). This is a
single-operator shared secret, not a user system — see `webui.md` §8 non-goals.

```bash
# Local (loopback, no auth, auto-opens browser)
peakbot --web

# Remote access (token required for non-loopback)
PEAKBOT_WEB_TOKEN=$(openssl rand -hex 16) peakbot --web --bind 0.0.0.0:7823
# then open  http://<host>:7823/?token=<the-secret>
```

### Testing & debugging the Web UI

The web UI is a JS SPA that talks to PeakBot over WebSocket. When iterating on
`src/ui/web/` or chasing a UI-only bug (layout, render, asset 404, WS
handshake, sticky-session reattach), load the [`playwright-cli`](.agents/skills/playwright-cli/SKILL.md)
skill — it is the recommended way to drive a real browser against
`peakbot --web`. It captures accessibility snapshots (better than screenshots
for verifying structure), records console + page error logs under
`.playwright-cli/`, and traces `/ws` traffic so you can confirm messages flow
on reconnect.

Typical loop:

```bash
# 1. start the web build (separate terminal — leave running)
make dev                   # backend on :8080, Vite HMR on :5173

# 2. drive a headless browser from the agent
playwright-cli open http://localhost:5173/?token=$PEAKBOT_WEB_TOKEN
playwright-cli snapshot     # see refs (e15, …) for every interactive element
playwright-cli click e15
playwright-cli console      # last console + page errors
```

For token-gated binds, prefer `PEAKBOT_WEB_TOKEN` over `--token` so the secret
stays out of shell history. When a WS bug is suspected, replay the run with
`playwright-cli` tracing on — it surfaces handshake, reattach, and close
frames that the server log alone doesn't show. For pure server-side WS work
(no UI), `websocat ws://localhost:7823/ws` is still the lightest probe.


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
    .tool(FileCreateTool)
    .tool(FileStrReplaceTool)
    .tool(FileInsertTool)
    .tool(BashTool)
    .build();
```

## File Map

```
src/
├── main.rs                 # Entry point, creates AgentRunner
├── lib.rs                  # AgentRunner, system prompt building, conversation conversion
├── bg_processes.rs         # BgRegistry, PTY-backed long-running processes for `bash_bg`
├── config.rs               # Configuration loading from config.yaml + env vars
├── context_manager.rs      # Context compaction for long conversations
├── conversation.rs         # Conversation data structures
├── conversation_manager.rs # Conversation persistence manager
├── vector/                 # Semantic document memory (doc_index / doc_search)
│   ├── mod.rs              # VectorStore (Arc<VectorDB> + EmbeddingsClient); open/index_file/search; id hashing; spawn_blocking
│   ├── embeddings.rs       # EmbeddingsClient -- reqwest → /v1/embeddings, batched, dim-validated
│   ├── parse.rs            # extract_text(path) -- dispatch by ext (txt/md/src/html/pdf/docx)
│   └── chunk.rs            # split(text) -- overlapping char windows (size+overlap constants)
├── system_prompt_persona.txt  # Crusader persona (agentless-only; dropped for the orchestrator)
├── system_prompt_core.txt     # Core tool guidance (agentless + orchestrator)
├── system_prompt_memory.txt   # memory.md workflow section (gated by memory.enabled)
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
│       ├── markdown.rs        # MarkdownRenderer (agent replies → headers/bold/italic/code/tables)
│       ├── message_renderer.rs # MessageRenderer trait, PlainRenderer
│       ├── render_cache.rs     # ChatRenderCache, WindowView
│       ├── repl_impl.rs        # ReplUi View (ratatui) + render loop
│       ├── spinner.rs          # Working-indicator spinner frames + elapsed formatter
│       └── todo_panel.rs       # TODO side-panel renderer
└── tools/
    ├── mod.rs              # Re-exports: all built-in tools
    ├── bash.rs             # BashTool -- shell execution with timeout
    ├── bash_bg.rs          # BashBgTool -- start/stop/list/send_line long-running PTY processes
    ├── doc_index.rs        # DocIndexTool -- parse/chunk/embed/store a file or dir (opt-in)
    ├── doc_search.rs       # DocSearchTool -- semantic top-k search over the store (opt-in)
    ├── fetch_url.rs        # FetchUrlTool -- HTTP GET requests
    ├── file_edit/          # File-editing tool family (shared helpers in mod.rs)
    │   ├── mod.rs              # MatchLevel, FileEditError, matching + IO helpers, tests
    │   ├── create.rs           # FileCreateTool -- create new file
    │   ├── str_replace.rs      # FileStrReplaceTool -- replace exact text
    │   └── insert.rs           # FileInsertTool -- insert at line
    ├── file_read.rs        # FileReadTool -- read with line ranges
    ├── pdf_read.rs         # PdfReadTool -- extract text/markdown from a PDF (pdf_oxide)
    ├── list_directory.rs   # ListDirectoryTool -- dir listing with recursion
    ├── search.rs           # SearchTool -- SearXNG web search
    ├── think.rs            # ThinkTool -- reasoning tool
    ├── thought_gate.rs     # ThoughtGate -- injects the `thought` field + soft nudge; wraps every built-in & MCP tool
    ├── todo.rs             # TodoTool -- todo list management
    ├── view_image.rs       # ViewImageTool -- load a local image into vision (Anthropic-only)
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

## Zen of Engineering Review (Mandatory)

**No implementation begins without a Zen review.** This is non-negotiable. Before writing any code — whether for a new feature, a bugfix, or a refactor — you must run the proposed change through the `zen-engineering` skill.

The Zen review enforces:
- **Simplicity first** — every abstraction, layer, and config option must earn its place.
- **No code before the plan is locked** — write the plan, get agreement, then code. Sunk cost is real; once implementation starts, the plan stops being a plan and becomes a description of what was already built.
- **What can be removed?** — identify dead code, redundant validation, premature abstractions, and over-engineered extensibility points before they ship.
- **What will confuse people?** — catch naming mismatches, surprising control flow, implicit contracts, and violations of the principle of least astonishment.
- **Are the boundaries clean?** — validation belongs at entry points, data models should make illegal states unrepresentable, and DRY applies only to things that are *necessarily* the same.
- **YAGNI** — don't build for hypothetical future needs. Build for what is needed now.

### How to run the review

Trigger the `zen-engineering` skill by asking for a review of the proposed design, architecture, or change. Provide enough context for a meaningful critique: the problem statement, the planned approach, and any tradeoffs you're considering.

**Do not skip this step.** The Zen review is the first line of defense against complexity creep, especially when working with AI-assisted coding. Models have a tendency to gold-plate, add error handling for impossible cases, and introduce patterns that weren't asked for. The Zen review exists to catch that before it becomes code.

If the review flags significant issues, address them in the plan before touching any source files. If the review surfaces new rule candidates for the skill, note them and propose additions.

---
## Comment Style

Code comments are signal, not narration. Keep them tight:

- **2–3 lines tops, ideally 1.** If a comment needs a paragraph, the code probably needs a refactor or the explanation belongs in a doc, not inline.
- **Explain *why*, not *what*.** The code already says what it does. Comment the non-obvious reason, the gotcha, the invariant.
- **No plans, stages, or temporal narration.** Never write comments like `// Step 1:`, `// now we…`, `// TODO later we'll…`, `// new approach`, `// changed to fix X`. These describe a moment in time that is already gone the instant the code lands — they rot immediately and mislead the next reader. Version control is the record of how the code evolved; comments are for the code as it is *now*.
- **Fix old mess-ups on sight.** When you touch a file and spot bloated, stale, or narrative comments, trim them as part of your change. Leave every file cleaner than you found it.

---
## Commit Procedure

**IMPORTANT: All changes must be made through Pull Requests.**

Every change to this codebase requires:
1. A **Pull Request** — no direct commits to main/master
2. **Changelog entry** in `release-notes/current.md`

### Changelog Requirements

Every PR must add an entry to `release-notes/current.md` describing what changed:

```markdown
## Changes

- Added new feature X
- Fixed bug Y
- Updated documentation
```

**`current.md` is the working draft for the *next* release.** It accumulates changes as they land. It is **not** what the release pipeline reads — the pipeline reads `release-notes/<version>.md`.

#### Release notes workflow (mandatory)

Before running `make release`, you **must** promote `current.md` to a versioned file. The release pipeline will fail to find notes if you skip this step.

```bash
# 1. Rename the working draft to the versioned file
mv release-notes/current.md release-notes/0.3.0.md

# 2. Commit the versioned file
git add release-notes/0.3.0.md
git commit -m "docs: 0.3.0 release notes"

# 3. Create a fresh empty current.md for the next cycle
cat > release-notes/current.md <<'EOF'
# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes
EOF
git add release-notes/current.md
git commit -m "docs: reset current.md for next release"

# 4. Now run the release — it will pick up release-notes/0.3.0.md
make release VERSION=0.3.0
```

**Why this matters:** `make release` reads the release body from `release-notes/<version>.md`. If that file does not exist, the release ships with a fallback body (`Release <version>`) and the changelog is lost from version control. The `current.md` file is never read by the release pipeline — it is only a human-edited staging area.

**Never commit changes without updating `current.md`.** This ensures every release has complete, version-controlled release notes.

### Pre-commit Gate
This is non-negotiable — the build pipeline treats clippy warnings as
real signals, the release flow rebuilds four platforms, and stale
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

### Internal Documentation

Analysis documents (plans, reviews, forensics, post-mortems, and similar
working notes) are **internal only** and must not be committed to the repo or
referenced in any public-facing content. They may exist on disk for working
purposes, but the repository is not their home.

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

- The release flow (`make release`) compiles four platforms from
  scratch. A warning that compiles fine on Linux can be a hard error on
  Windows or macOS.
- The `-D warnings` gate means *any* new warning blocks the commit.
  This forces the warning to be either fixed or explicitly allowed at
  the moment it's introduced — not three releases later when nobody
  remembers the context.
- `cargo fmt` keeps diffs minimal and reviewable. Unformatted commits
  fight against every future `git blame`.

## Build & Release

PeakBot ships as a single static binary for **Linux x86_64**, **Windows x86_64**, **macOS universal2** (Intel + Apple Silicon in one fat binary), and **Android arm64** (a terminal binary for Termux / `adb shell` — not an APK). All four are produced from Linux via container builds — no native macOS/Windows/Android host required. The driver is a top-level `Makefile`; cross-compilation is handled by four sibling Dockerfiles.

### Dockerfiles

| File | Builder image | Default `TARGET` | Notes |
|------|---------------|------------------|-------|
| `Dockerfile.linux` | `rust:1.88-bookworm` | `x86_64-unknown-linux-gnu` | Native build inside Debian; uses dummy-main caching trick |
| `Dockerfile.windows` | `rust:1.88-bookworm` + mingw | `x86_64-pc-windows-gnu` | Cross via gcc-mingw; final stage `FROM scratch` |
| `Dockerfile.macos` | `ghcr.io/rust-cross/cargo-zigbuild:latest` | `universal2-apple-darwin` | Cross via zig + bundled macOS SDK; produces fat binary (lipo merge) |
| `Dockerfile.android` | `rust:1.91-bookworm` + Android NDK r27c | `aarch64-linux-android` | Cross via `cargo-ndk` (NDK clang linker). API level via `CARGO_NDK_PLATFORM` (24), NOT the `-p` flag — cargo-ndk v4's `-p` collides with cargo's `--package`. Links only bionic (libc/libm/libdl). Works clean because PeakBot is pure rustls/ring (no OpenSSL). |

All four use a `FROM scratch` final stage and `--output type=local,dest=./output` so `make` extracts only the binary into `output/` without leaving an image in the local registry. Override the rust target with `--build-arg TARGET=...` if you need a single-arch macOS or a different libc.

> **Gotcha:** `ARG` does not propagate across `FROM` stage boundaries. Each Dockerfile redeclares `ARG TARGET=...` in its scratch stage so the `COPY --from=builder target/${TARGET}/release/...` path interpolates correctly.

### Make targets

Run `make help` for the full list. Day-to-day:

| Target | What it does |
|--------|--------------|
| `make` / `make build` | Build all four platforms in sequence (linux, windows, macos, android) |
| `make build-linux` | Build `output/peakbot-linux-amd64` (Linux x86_64) |
| `make build-windows` | Build `output/peakbot-windows-amd64.exe` (Windows x86_64) |
| `make build-macos` | Build `output/peakbot-macos-universal2` (macOS universal2) |
| `make build-android` | Build `output/peakbot-android-arm64` (Android arm64, Termux/`adb shell`) |
| `make clean` | `rm -rf output/` |
| `make rebuild` | `clean` + `build` (rebuilds all four) |
| `make help` | Print this table from `## ` doc comments in the Makefile |
| `make web` | Build the web UI bundle (`web/dist/`) for `peakbot --web` |
| `make dev` | Web UI dev loop: backend under `cargo watch` (`:8080`) + Vite HMR (`:5173`) together. Open `localhost:5173`; Vite proxies `/ws` to the backend. Web edits hot-swap <1s; Rust edits rebuild+restart (drops the WS session). Requires `cargo-watch` + Node 22+. Touches no Rust code paths — production is unchanged. |

Non-release builds produce **unversioned** filenames (e.g. `peakbot-linux-amd64`). The release flow injects the semver via `--build-arg VERSION=$v`, which the Dockerfiles splice into the artifact name as `peakbot-<v>-<platform>`. With `VERSION` unset (the default), the conditional `${VERSION:+-${VERSION}}` substitution in each Dockerfile collapses to nothing — same `cargo build --release`, just a cleaner name.

`CONTAINER_BUILDER` auto-detects `podman` (preferred) and falls back to `docker`. Override with `make build CONTAINER_BUILDER=docker`.

### Release pipeline

The `make release` target runs the full release flow end-to-end:

```
release-bump → release-tag → release-build-linux → release-build-windows → release-build-macos → release-build-android → release-publish
```

Each phase does one thing and can be re-run independently if a later phase fails. In-flight state is stashed in `.release-version` (gitignored) and deleted after a successful publish.

| Phase | Action |
|-------|--------|
| `release-bump` | Validate semver, refuse if tag exists, refuse on dirty tree (override with `ALLOW_DIRTY=1`), rewrite `[package].version` in `Cargo.toml`, sync `Cargo.lock` via `cargo update -p peakbot --precise <v>`, commit `chore: release <v>` |
| `release-tag` | Create annotated tag `<v>` (bare semver, no `v` prefix), push current branch + tag to `origin` |
| `release-build-{linux,windows,macos,android}` | Run the matching Docker build, copy artifact to `output/peakbot-<v>-{linux-amd64,windows-amd64.exe,macos-universal2,android-arm64}` |
| `release-publish` | Create a Gitea release via REST API and upload all four asset files |

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
- `podman` or `docker` — to run the four Dockerfile builds
- `curl`, `jq` — used by `release-publish` to talk to Gitea
- `awk` — portable invocation only (no PCRE lazy quantifiers); works under gawk / mawk / busybox awk

#### Release notes

The Gitea release body and the annotated tag message are populated from a
Markdown file, resolved in this order:

1. `NOTES=<path>` on the make CLI (e.g. `make release VERSION=0.2.0 NOTES=/tmp/notes.md`)
2. `release-notes/<v>.md` in the repo (the default convention — version-controlled, PR-reviewable)
3. The literal string `Release <v>` if neither exists

**The release pipeline reads `release-notes/<v>.md`, never `current.md`.**
See [Release notes workflow (mandatory)](#release-notes-workflow-mandatory) above
for the full pre-release steps.

Notes are optional — a release with no notes still ships; you'll see an
`ℹ️  No release-notes file …` message and the default body is used. To
ship notes, promote `current.md` to `release-notes/<v>.md` *before* running
`make release`. The same content is used for both the Gitea release page
and `git show <v>`.

The notes file is read via `jq --rawfile`, so any Markdown content is
safe (newlines, backticks, quotes, `$`) — no shell escaping required.

```bash
# Conventional flow (recommended) — see Changelog Requirements for full steps
make release VERSION=0.3.0          # picks up release-notes/0.3.0.md automatically

# Ad-hoc override
make release VERSION=0.3.0 NOTES=/tmp/generated-notes.md

# Hotfix without notes — pipeline still ships, body falls back to "Release 0.3.1"
make release VERSION=0.3.1
```

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

**Exception — the `anthropic` provider gates on transport, not model name.**
The Anthropic Messages transport carries images natively, so `[img:…]` is
accepted on the `anthropic` provider for *any* model name (mirrors how the
`view_image` tool is gated — see `providers::supports_vision_for`). This is
deliberate: a local llama-server GGUF or a gateway model (e.g.
`minimax/MiniMax-M3`) has a name the conservative detector can't recognise,
and blocking a capable model pre-flight is worse than letting a genuinely
non-vision model reply "I can't see images". Every other provider keeps the
name-based detection above.

**Per-model override — `vision: true` / `vision: false`.** Any model entry can
set an explicit `vision:` flag that overrides auto-detection (see
[per-model overrides](#multi-model-with-model)). `true` forces image support
on (enables `[img:…]`, and on the Anthropic provider also registers the
`view_image` tool); `false` forces it off. Omit the flag for the auto
behaviour described above. This is the resolution implemented by
`providers::resolve_supports_vision(override, provider, model)` — a single
point feeding both the `[img:…]` gate and `view_image` registration. Because
`view_image` only delivers images on the Anthropic transport, `vision: true`
on a non-Anthropic model enables `[img:…]` but does not register `view_image`.

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


## Multi-agent pipeline (orchestrator + sub-agents)

PeakBot can run as an **orchestrator** that delegates tasks to **sub-agents**.
You talk to one agent in one conversation; it spawns focused sub-agents,
each with its own fresh context, and you see every sub-agent's turns tagged by
role. This is opt-in via the `pipeline:` config block — when it's absent or
`enabled: false`, none of this exists and the `delegate` tool isn't registered.
A runnable example lives under [`examples/pipeline-team/`](examples/pipeline-team/).

### Per-conversation opt-in (default off)

Configuring `pipeline:` makes sub-agents **available**, not automatically **on**.
Each conversation opts in independently, and the choice is **off by default**:

- The web **Agents** panel has an *Enable subagents* checkbox; the terminal has
  **`/subagents on|off`**. Both route to the same seam.
- The choice is **mutable only before the first turn** — once the conversation
  has a real turn it is **locked** (flipping the `delegate` tool mid-conversation
  would desync the tool list from the wire history). Start a new conversation to
  change it.
- Toggling reuses the agent-rebuild seam (`rebuild_agent_for_resolved`, shared
  with `/model` and `/cd`): it rebuilds the agent on the *same* model/cwd with
  `delegate` added or removed — no conversation reset.
- The opt-in persists on the conversation (`subagents_enabled`, serde-default
  false), so a resumed opt-in conversation rebuilds with `delegate`.
- Two distinct facts drive this: `pipeline_available` (config, boot-only — is a
  pipeline configured?) and `subagents_enabled` (per-conversation user choice).
  The `delegate` tool registers iff **both** are true — gated in one place
  (`session::create_session` at boot, `rebuild_agent_for_resolved` on rebuild).

### The model

- **You talk to one agent: the orchestrator.** The orchestrator is simply your
  normal top-level agent (whatever `default_model` resolves to). It is **not** a
  pipeline role — never list it under `pipeline.agents`.
- The orchestrator gets a **`delegate(role, task)`** tool. Each call runs one
  **sub-agent** — a role defined as *(model alias, prompt)* + optional `env:` —
  to completion on a **fresh context** (role prompt + that one task + its own
  tool loop), then returns **one string**.
- **Sequential only.** `delegate` runs one sub-agent at a time; the orchestrator
  sequences by calling it again. There is no parallel mode — the single
  role/single task tool signature makes "parallel" unrepresentable.
- **Fresh per delegation.** A sub-agent has no memory of prior delegations or of
  the conversation. Put everything it needs into `task`.

### What each party sees (the isolation invariant)

Three things that a plain single-agent chat conflates are pulled apart here:

1. **Display transcript** — what you see and what is persisted. The union of the
   orchestrator's turns *and* every sub-agent's turns, each tagged by origin
   (`MessageSource::SubAgent { role }`).
2. **Orchestrator wire context** — what is sent to the orchestrator model. Its
   own turns plus each delegate `ToolCall` + the returned result string. **Never**
   a sub-agent's internal turns.
3. **Sub-agent wire context** — what is sent to a sub-agent model. That role's
   prompt + the one task + its own tool round-trips. Thrown away after the call.

The load-bearing guarantee: sub-agent turns are appended to the transcript for
display/persistence but **filtered out of the orchestrator's wire history** by a
single lane filter in `get_agent_history`
(`msg.source.is_orchestrator_lane()`). Without it, a sub-agent's internal turns
would leak into the orchestrator's context on the next turn and on resume. This
is the single most important correctness check and has a dedicated regression
test (`get_agent_history_excludes_sub_agent_lane_keeps_background`). The
orchestrator's *own* `delegate` ToolCall/ToolResult stay on the orchestrator lane
— that's exactly the input+output it should see.

### Display: lane colouring

Sub-agent turns render with a distinct prefix (`🧩 <role>`) and colour, keyed on
`MessageSource` in `PlainRenderer` — the same mechanism `Background` turns use.
The web UI mirrors this via the `sub_agent` wire source in `Message.tsx`.

### Cost roll-up

A delegation's tokens and cost roll into the parent `/stats` — sub-agent usage is
accounted lane-agnostically at the event boundary, so delegating heavy work is
never silently free. (Exception: **Ollama** sub-agents are hookless — Ollama has
no event channel — so their turns are not TEE'd and their cost is not tracked.)

### Config shape

A role is `(model, prompt)` + optional `env:` + optional `skills:` + optional
`agents_md:`. The `model:`
names an **alias** from the top-level `providers:` list — the same aliases
`/model` uses — resolved at load against the `ModelRegistry`. Omit `model:` to
fall back to `default_model`. There are no provider/credential fields on a role;
model, key, and base URL all come from the resolved alias.

`pipeline.orchestrator_prompt` (optional) is extra framing appended to the
**orchestrator's** system prompt when sub-agents are active — see
[The orchestrator vs. sub-agent prompt](#the-orchestrator-vs-sub-agent-prompt).

```yaml
providers:
  - name: openrouter
    type: openrouter
    api_key: sk-or-v1-xxx
    models:
      - { name: anthropic/claude-3.7-sonnet, alias: sonnet, max_tokens: 8192 }
      - { name: google/gemini-2.0-flash-001, alias: flash }
default_model: sonnet          # ← the orchestrator you chat with

pipeline:
  enabled: true
  orchestrator_prompt: |         # optional; appended to the orchestrator prompt
    You lead a small team. Prefer delegating research and review to sub-agents;
    keep the main thread on planning and integration.
  agents:
    researcher:
      model: flash             # alias from providers: (falls back to default_model if omitted)
      prompt: "You research codebases and the web. Return a tight brief."
      skills:                  # optional per-role skill gate (mirrors `tools:`)
        only: [helius-solana, github]   # allowlist (XOR `disabled:`)
    reviewer:
      model: sonnet
      prompt: "You review diffs and critique."
      env: { REVIEW_STRICT: "1" }   # optional; merged into THIS sub-agent's bash env only
      agents_md: true          # optional; inject the repo's agents.md (default: off)
      skills:
        enabled: false         # this role gets NO skills section
```

Validation is at load (parse-at-the-boundary): an unknown alias, an empty
prompt, an empty role name, or a `skills:` filter that sets both lists or names
an unknown skill is a clear boot-time error. `pipeline:` is **boot-only** — edit
and restart (it's in the "ignored — restart to apply" set of the
[reload table](#live-reload-on-session-verbs-new-model-cd-load)).

### The orchestrator vs. sub-agent prompt

The system prompt has three recipes, all composed from the same small pieces
(`src/lib.rs::build_system_prompt` + `env_block`):

| Piece | Agentless | Orchestrator (sub-agents on) | Sub-agent |
|-------|:---------:|:----------------------------:|:---------:|
| persona (`system_prompt_persona.txt`, the "code crusader") | ✅ | ❌ | ❌ |
| core tool guidance (`system_prompt_core.txt`) | ✅ | ✅ | ❌ |
| memory.md workflow | ✅ (if enabled) | ✅ (if enabled) | ❌ |
| skills section | ✅ (all) | ✅ (all) | ⚙️ per-role filtered |
| env block (cwd/time/OS/shell) | ✅ | ✅ | ✅ |
| `agents.md` | ✅ | ✅ | ⚙️ per-role (`agents_md:`, default off) |
| `orchestrator_prompt` | — | ✅ (if set) | — |

- **The orchestrator drops the crusader persona** in sub-agents mode — it would
  confuse an agent whose job is to coordinate a team — but keeps the core tool
  guidance and gains the optional `orchestrator_prompt`. Agentless mode is
  byte-identical to before this feature.
- **A sub-agent's preamble** is `role.prompt` + the live env block + its
  per-role-filtered skills, plus the repo's `agents.md` **only if the role sets
  `agents_md: true`** (default off). Otherwise its `role.prompt` is its whole
  persona, and everything else it needs goes in the delegated `task`. Built
  fresh per delegation (`build_sub_agent_preamble`), so cwd/time are current.
- **Per-role skills** (`pipeline.agents.<role>.skills`) mirror the `tools:`
  filter: `only:` (allowlist) XOR `disabled:` (blocklist), plus an `enabled:`
  master switch (`false` ⇒ no skills for that role). `SkillFilter::shows`
  decides visibility; `SkillFilter::validate` runs at boot against the
  discovered skill names.
- **Per-role `agents_md`** (`pipeline.agents.<role>.agents_md`, bool, default
  `false`) opts a sub-agent into the repo's `agents.md`. The orchestrator and
  agentless agent always get it; sub-agents stay lean unless a role opts in,
  reusing the same `agents_md_section(cwd)` the main prompt uses (derived at
  delegation time, so cwd is current).
- The persona/orchestrator framing is recomputed at the **single agent-rebuild
  seam** (`rebuild_agent_for_resolved`), keyed on the live
  `pipeline_available && subagents_enabled` state, so toggling sub-agents,
  `/cd`, and `/model` all produce the right prompt without any caller rebuilding
  it themselves.

### Tools & isolation (v1)

- Every sub-agent receives the **full built-in toolset MINUS `delegate`** — the
  one deliberate subtraction (no nested delegation). It can read files, run
  `bash`/`bash_bg`, search the web, etc.
- **No harness sandbox in v1.** A sub-agent can write and run bash. Isolation is
  a **configuration** concern, not a safety gate — a top-level tool-disable
  feature (planned separately) will scope which tools a role gets once it exists;
  there is intentionally no bespoke per-role tool filter today.
- `env:` is per-role, merged only into that sub-agent's bash tool env — it never
  leaks into the orchestrator or other roles.
- `skills:` is per-role and affects only what the sub-agent *sees listed* in its
  preamble — it does not change which tools are registered.

### Stop during a delegation

Hitting stop while a sub-agent runs aborts the **whole turn** — the sub-agent turn
and the orchestrator turn unwind together ("stop means stop"). Stop routes to the
innermost running hook (the sub-agent's, tracked in a shared `ActiveSubAgentHook`
cell) *and* the orchestrator's; there is no "delegation stopped, back to
orchestrator" resumption path.

### Where it lives

- `src/pipeline/delegate_tool.rs` — the `delegate` tool + `SubAgentDeps` build
  context + stop routing.
- `src/pipeline/registry.rs` — `SubAgentRegistry`: resolves each role's alias
  against the `ModelRegistry` at construction.
- `src/providers/mod.rs` — `build_sub_agent` builds a live sub-agent beside
  `create_*_agent`, sharing `add_builtin_tools` (the real shared piece).
- `MessageSource::SubAgent { role }` + `is_orchestrator_lane()`
  (`src/ui/app_state.rs`); the lane filter in `get_agent_history`
  (`src/state/state_manager.rs`).

This is the concrete, config-driven form of the generic rig agents-as-tools
pattern described under [Multi-agent patterns](#multi-agent-patterns).

## CI (Gitea Actions)

The repo runs CI on **Gitea Actions** via the workflow at
[`.gitea/workflows/ci.yml`](./.gitea/workflows/ci.yml). Single job,
single runner (`shinpachi`, label `dind`), `rust:1.95` container. The
gate is: `cargo fmt --all -- --check` → `cargo clippy --all-targets
--locked -- -D warnings` → `cargo test --workspace --locked`.

### Reproducing the gate locally

This is the same three commands CI runs. Run them from the repo root
before pushing:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Autofix pass when fmt/clippy flag something:

```bash
cargo fmt --all
cargo clippy --all-targets --fix --allow-dirty
cargo clippy --all-targets --fix --allow-dirty --tests
# then re-verify with the gate above
```

### Inspecting a failed run

`GITEA_URL` / `GITEA_TOKEN` are already in the PeakBot shell env (confirm
with `env | grep -i gitea`). Owner is `ai-bots`, repo is `peakbot`.

**Step 1 — list latest runs:**

```bash
curl -s -H "Authorization: token $GITEA_TOKEN" \
  "$GITEA_URL/api/v1/repos/ai-bots/peakbot/actions/tasks?page=1&limit=5" \
  | jq '.workflow_runs[] | {id, run_number, status, conclusion, head_sha: .head_sha[0:8], display_title}'
```

Key fields: `id` (internal — used everywhere below), `run_number` (the
UI number in the web URL), `status`, `conclusion`, `head_sha`.

> ⚠️ **Gitea ID quirk:** `workflow_runs[*].url` looks like
> `…/actions/runs/{run_number}` but every API endpoint below wants the
> internal `id`. UI run #3 ≠ internal id. **Always use `id`.**

**Step 2 — find the job id:**

The expected path `/actions/runs/{id}/jobs` returns an empty list on
our Gitea version. The path that actually works is:

```bash
curl -s -H "Authorization: token $GITEA_TOKEN" \
  "$GITEA_URL/api/v1/repos/ai-bots/peakbot/actions/jobs?run=<anything>" \
  | jq '.jobs[] | {id, run_id, head_sha: .head_sha[0:8], status, conclusion}'
```

The `?run=` filter is **ignored** — it returns every job in the repo.
Match on `head_sha` to find the one you want.

**Step 3 — pull the log:**

```bash
JOB_ID=<id from step 2>
curl -s -H "Authorization: token $GITEA_TOKEN" \
  "$GITEA_URL/api/v1/repos/ai-bots/peakbot/actions/jobs/$JOB_ID/logs" \
  -o /tmp/peakbot/job_${JOB_ID}.log
```

`404 "job not found"` means you have the wrong id (probably the run id
instead of the job id, or a stale `run_number`).

**Step 4 — extract the failure:**

```bash
grep -E "::group|::endgroup|::error|Run Main|Process completed|fatal|error\[|test result|FAILED|panicked" \
  /tmp/peakbot/job_${JOB_ID}.log | tail -n 60
```

The literal error is right before `##[error]Process completed with exit
code N.` Read upward from there to the most recent `::group::Run …` to
identify the failing step.

**Step 5 — poll for the new run after pushing a fix:**

```bash
TARGET_SHA=$(git rev-parse --short HEAD)
for i in $(seq 1 90); do
  line=$(curl -s -H "Authorization: token $GITEA_TOKEN" \
    "$GITEA_URL/api/v1/repos/ai-bots/peakbot/actions/tasks?page=1&limit=1" \
    | python3 -c "import sys,json; r=json.load(sys.stdin)['workflow_runs'][0]; \
                  print(r['head_sha'], r['status'], r.get('conclusion','') or '-', r['run_number'])")
  echo "[poll $i] $line"
  sha=$(echo "$line" | awk '{print $1}')
  state=$(echo "$line" | awk '{print $2}')
  if [ "${sha:0:7}" = "$TARGET_SHA" ] && \
     { [ "$state" = "success" ] || [ "$state" = "failure" ] || [ "$state" = "cancelled" ]; }; then
    echo "DONE: $state"; break
  fi
  sleep 15
done
```

Warm rust:1.95 container: ~90 s. Cold: 3–6 min. Don't poll faster than
every 10 s.

### Sanity endpoints

```bash
# Runners visible to this repo / org
curl -s -H "Authorization: token $GITEA_TOKEN" \
  "$GITEA_URL/api/v1/repos/ai-bots/peakbot/actions/runners" | jq
curl -s -H "Authorization: token $GITEA_TOKEN" \
  "$GITEA_URL/api/v1/orgs/ai-bots/actions/runners" | jq
```

If both return empty, jobs sit `cancelled`/`queued` forever even with a
valid workflow YAML. Currently `shinpachi` is registered at the
**instance** level (not visible via the repo/org endpoints above — both
return empty), and advertises the label `dind`.

### Gitea-Actions vs GitHub-Actions: the must-knows

Hard-won during CI bring-up:

1. **`actions/checkout` is mandatory.** The workspace volume starts
   empty — no auto-clone. We use a pure-git fallback so we don't need
   `node` in the container:
   ```yaml
   - run: |
       git init -q
       git fetch --depth=1 \
         "https://x-access-token:${GITHUB_TOKEN}@${GITHUB_SERVER_URL#https://}/${GITHUB_REPOSITORY}.git" \
         "$GITHUB_SHA"
       git checkout --detach FETCH_HEAD
   ```
2. **JS actions need `node` in the container.** Slim images (`rust`,
   `alpine`, etc.) don't have it. Either bring node, install it, or
   replace JS actions with shell steps (recommended).
3. **`GITHUB_TOKEN` is NOT auto-exported into `run:` step env.** Mirror
   it explicitly: `env: { GITHUB_TOKEN: ${{ github.token }} }`.
4. **Toolchains arrive minimal.** Official `rust:*` images ship only
   `cargo`/`rustc`/`rust-std`. Add `rustfmt`/`clippy` via
   `rustup component add` if the gate uses them.
5. **`runs-on:` must match an existing runner's advertised labels.**
   Currently our runner advertises `dind`.
6. **Tests that need network / external binaries must be `#[ignore]`d.**
   The container has no `uvx`, `npm`, `python`, or general egress. Mark
   such tests with `#[ignore = "reason"]` and run locally with
   `cargo test -- --ignored`. (See the 2026-05-25 lesson where three
   `test_connect_mcp_server_*` tests panicked in CI because they spawn
   `uvx` to fetch an MCP server from GitHub.)
7. **Lift `working-directory:` only if the subdir exists.** When
   copying a workflow between repos, the `working-directory: backend`
   from a monorepo will fail with `chdir … no such file or directory`
   in a flat-layout repo. peakbot has `Cargo.toml` at the root — no
   `working-directory` is needed.
