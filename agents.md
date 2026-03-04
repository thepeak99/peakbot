# agents.md

## Overview

PeakBot is a single-agent coding assistant built with [Rig](https://github.com/0xPlaygrounds/rig) (`rig-core` v0.31). It runs as a terminal REPL, backed by Anthropic's Claude Sonnet 4, equipped with filesystem and shell tools to read, write, and execute code on the user's machine.

## Architecture

```
User (stdin)
  │
  ▼
┌──────────────────────────────────────────────┐
│  REPL Loop  (src/main.rs)                    │
│  Reads input, passes to agent, prints output │
│  Maintains chat_history: Vec<Message>        │
└──────────────┬───────────────────────────────┘
               │
               ▼
┌──────────────────────────────────────────────┐
│  Rig Agent                                   │
│  Model: claude-sonnet-4-0                    │
│  System prompt: src/system_prompt.txt        │
│  Max tokens: 4096                            │
│  Max tool turns per message: 15              │
│                                              │
│  Agentic loop (managed by Rig):              │
│  1. Send prompt + tool defs to Claude        │
│  2. If Claude returns tool_calls:            │
│     a. Deserialize args                      │
│     b. Execute tool.call(args)               │
│     c. Serialize output as ToolResult        │
│     d. Append to history, goto 1             │
│  3. If Claude returns text: return to REPL   │
└──────────────┬───────────────────────────────┘
               │
               ▼
┌──────────────────────────────────────────────┐
│  Tool Set                                    │
│                                              │
│  ┌─────────────┐  ┌─────────────┐           │
│  │ file_edit    │  │ file_read   │           │
│  │ 4 commands   │  │ line ranges │           │
│  └─────────────┘  └─────────────┘           │
│  ┌─────────────┐  ┌────────────────┐        │
│  │ bash        │  │ list_directory │        │
│  │ /bin/sh -c  │  │ recursive opt  │        │
│  └─────────────┘  └────────────────┘        │
└──────────────────────────────────────────────┘
```

## Agent

There is one agent. It is constructed in `src/main.rs`:

```rust
let agent = client
    .agent(model_name)  // configurable via config.yaml
    .preamble(SYSTEM_PROMPT)                        // loaded from src/system_prompt.txt
    .max_tokens(config.openrouter.max_tokens)       // configurable via config.yaml
    .tool(FileEditTool::default())
    .tool(FileReadTool)
    .tool(BashTool)
    .tool(ListDirectoryTool)
    .build();
```

The agent uses **OpenRouter** as the provider (migrated from Anthropic). All settings are configurable via `config.yaml` or environment variables.

The agent is stateless between tool turns -- all state lives in `chat_history` (conversation memory) and `FileEditTool.file_history` (undo stack). Rig owns the agentic loop: it automatically dispatches tool calls, collects results, and loops until the model produces a text response or hits the 15-turn limit.

## Tools

All tools live in `src/tools/` and implement `rig::tool::Tool`. Each tool defines:
- `NAME` -- unique string identifier sent to the model
- `Args` -- struct deserialized from JSON the model produces
- `Output` -- type serialized back to the model as a tool result
- `Error` -- tool-specific error type (errors are sent back to the model, not fatal)
- `definition()` -- returns the JSON Schema the model uses to know what to send
- `call()` -- executes the tool logic

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

## Data Flow

1. **User types a message** in the terminal.
2. `agent.prompt(input).with_history(&mut chat_history).max_turns(15).await` enters Rig's agentic loop.
3. Rig builds a `CompletionRequest` with the system prompt, chat history, user message, and all 4 tool definitions (JSON Schemas).
4. Rig sends it to Claude via the Anthropic API.
5. Claude responds with either text (done) or tool calls.
6. For each tool call, Rig deserializes `Args` from the JSON the model produced, calls `tool.call(args)`, and serializes `Output` back to JSON.
7. Tool results are appended to the conversation as `ToolResult` messages.
8. Rig sends the updated conversation back to Claude (goto step 4).
9. When Claude produces a text response, Rig returns it to the REPL.
10. The REPL prints the response and `chat_history` now contains the full exchange for the next turn.

## Error Handling

- **Tool errors** (bad path, non-unique match, timeout) are returned to the model as tool results. The model sees the error message and self-corrects. These do not crash the agent.
- **API errors** (auth failure, network) surface as `PromptError` and are printed in the REPL. The loop continues.
- **Max turns exceeded** means the model used all 15 tool rounds without producing a final answer. Printed as an error; the user can retry or rephrase.

## Configuration

| Setting | Value | Where |
|---------|-------|-------|
| Model | `claude-sonnet-4-0` | `src/main.rs:24` |
| Max tokens | 4096 | `src/main.rs:26` |
| Max tool turns | 15 | `src/main.rs:59` |
| Output truncation | 10,000 chars | each tool file |
| Bash timeout | 30s default, 120s max | `src/tools/bash.rs:9-10` |
| Recursive dir depth | 3 | `src/tools/list_directory.rs:77` |

Environment: set `ANTHROPIC_API_KEY` before running.

## Extending

### Adding a new tool

1. Create `src/tools/my_tool.rs`
2. Define `MyToolArgs` (derive `Deserialize`), `MyToolError` (derive `thiserror::Error`), and `MyTool` (derive `Serialize, Deserialize`)
3. Implement `rig::tool::Tool` for `MyTool`:
   - Set `NAME`, `Error`, `Args`, `Output`
   - Return a `ToolDefinition` with JSON Schema in `definition()`
   - Implement logic in `call()`
4. Add `mod my_tool;` and `pub use my_tool::MyTool;` in `src/tools/mod.rs`
5. Add `.tool(MyTool)` to the agent builder in `src/main.rs`

### Switching models

Change the model in `config.yaml` or set the `PEAKBOT_OPENROUTER_MODEL` environment variable. Find models at https://openrouter.ai/models.

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
├── main.rs                 # Agent construction, REPL loop, entry point
├── config.rs               # Configuration loading from config.yaml
├── system_prompt.txt       # System prompt (included at compile time)
└── tools/
    ├── mod.rs              # Re-exports: BashTool, FileEditTool, FileReadTool, ListDirectoryTool
    ├── file_edit.rs        # FileEditTool -- view/create/str_replace/insert (437 lines)
    ├── file_read.rs        # FileReadTool -- read with line ranges (122 lines)
    ├── bash.rs             # BashTool -- shell execution with timeout (120 lines)
    └── list_directory.rs   # ListDirectoryTool -- dir listing with recursion (127 lines)
```

## Configuration

All settings are managed via environment variables:

| Environment Variable | Default |
|---------------------|---------|
| `OPENROUTER_API_KEY` | (required) |
| `OPENROUTER_MODEL` | `anthropic/claude-3.7-sonnet` |
| `OPENROUTER_MAX_TOKENS` | `4096` |
| `AGENT_MAX_TURNS` | `15` |
