# PeakBot: Coding Agent Implementation Plan

A simple coding agent built with [Rig](https://github.com/0xPlaygrounds/rig) (v0.31.0) that can edit files, read files, run shell commands, and list directories. Uses Anthropic's Claude as the backing LLM.

---

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [Dependencies](#2-dependencies)
3. [Project Structure](#3-project-structure)
4. [Tool Definitions](#4-tool-definitions)
   - 4.1 [FileEditTool](#41-fileedittool)
   - 4.2 [FileReadTool](#42-filereadtool)
   - 4.3 [BashTool](#43-bashtool)
   - 4.4 [ListDirectoryTool](#44-listdirectorytool)
5. [Agent Setup](#5-agent-setup)
6. [Interactive REPL Loop](#6-interactive-repl-loop)
7. [Error Handling Strategy](#7-error-handling-strategy)
8. [File-by-File Implementation Guide](#8-file-by-file-implementation-guide)
9. [System Prompt](#9-system-prompt)
10. [Testing](#10-testing)

---

## 1. Architecture Overview

```
┌─────────────────────────────────────────────┐
│                  main.rs                     │
│  ┌─────────────────────────────────────────┐ │
│  │          Interactive REPL Loop          │ │
│  │  stdin -> agent.prompt() -> stdout      │ │
│  └────────────────┬────────────────────────┘ │
│                   │                          │
│  ┌────────────────▼────────────────────────┐ │
│  │         Rig Agent (Claude)              │ │
│  │  preamble + tools + agentic loop        │ │
│  └────────────────┬────────────────────────┘ │
│                   │                          │
│  ┌────────────────▼────────────────────────┐ │
│  │             Tool Set                     │ │
│  │  ┌──────────┐ ┌──────────┐              │ │
│  │  │FileEdit  │ │FileRead  │              │ │
│  │  └──────────┘ └──────────┘              │ │
│  │  ┌──────────┐ ┌──────────────┐          │ │
│  │  │  Bash    │ │ListDirectory │          │ │
│  │  └──────────┘ └──────────────┘          │ │
│  └─────────────────────────────────────────┘ │
└─────────────────────────────────────────────┘
```

The agent runs in a terminal REPL. The user types a message, the Rig agentic loop sends it to Claude, Claude may call tools (potentially multiple rounds), and eventually returns a text response.

Rig handles the entire agentic loop automatically: it sends the prompt to the model, if the model responds with tool calls Rig executes them, feeds results back, and repeats until the model returns a final text response (or max turns is hit).

---

## 2. Dependencies

Replace the contents of `Cargo.toml` with:

```toml
[package]
name = "peakbot"
version = "0.1.0"
edition = "2024"

[dependencies]
rig-core = "0.31"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
```

### Why each dependency

| Crate | Purpose |
|-------|---------|
| `rig-core` | The Rig framework. Published as lib name `rig`. Provides the `Tool` trait, `Agent`, Anthropic provider, agentic loop. |
| `tokio` | Async runtime. Rig requires tokio for async tool execution and HTTP. |
| `serde` / `serde_json` | Serialization. Tool `Args` must implement `Deserialize`, tool `Output` must implement `Serialize`. Tool definitions use `serde_json::json!`. |
| `thiserror` | Ergonomic error types for each tool's `Error` associated type. |
| `anyhow` | Top-level error handling in `main`. |
| `tracing` / `tracing-subscriber` | Logging. Rig uses `tracing` internally; this lets us see debug output. |

### Environment Variable

The agent requires `ANTHROPIC_API_KEY` to be set. The Anthropic client reads it automatically via `anthropic::Client::from_env()`.

---

## 3. Project Structure

```
peakbot/
├── Cargo.toml
├── src/
│   ├── main.rs              # Entry point: client setup, agent build, REPL loop
│   └── tools/
│       ├── mod.rs            # Re-exports all tools
│       ├── file_edit.rs      # FileEditTool (view, create, str_replace, insert)
│       ├── file_read.rs      # FileReadTool (read file contents)
│       ├── bash.rs           # BashTool (run shell commands)
│       └── list_directory.rs # ListDirectoryTool (list files/dirs)
```

---

## 4. Tool Definitions

Each tool implements Rig's `Tool` trait:

```rust
pub trait Tool: Sized + Send + Sync {
    const NAME: &'static str;
    type Error: std::error::Error + Send + Sync + 'static;
    type Args: for<'a> Deserialize<'a> + Send + Sync;
    type Output: Serialize;

    async fn definition(&self, _prompt: String) -> ToolDefinition;
    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error>;
}
```

Key pattern: `Args` is what the LLM sends (deserialized from JSON), `Output` is what goes back to the LLM (serialized to JSON string).

---

### 4.1 FileEditTool

**File: `src/tools/file_edit.rs`**

This is the most complex tool. It supports four commands modeled after Claude's `text_editor` tool: `view`, `create`, `str_replace`, and `insert`.

#### Args struct

```rust
#[derive(Deserialize)]
struct FileEditArgs {
    /// The command to execute: "view", "create", "str_replace", or "insert"
    command: String,
    /// Absolute path to the file
    path: String,
    /// For "create": the full file content to write
    file_text: Option<String>,
    /// For "view": optional [start_line, end_line] (1-indexed, -1 means EOF)
    view_range: Option<Vec<i32>>,
    /// For "str_replace": the exact string to find (must be unique in file)
    old_str: Option<String>,
    /// For "str_replace": the replacement string (None or absent = delete)
    new_str: Option<String>,
    /// For "insert": line number to insert after (0 = beginning of file)
    insert_line: Option<usize>,
    /// For "insert": the text to insert
    insert_text: Option<String>,
}
```

#### Tool definition (JSON Schema)

```rust
ToolDefinition {
    name: "file_edit".to_string(),
    description: "A filesystem editor tool. Supports four commands:\n\
        - `view`: View file contents or list directory (with optional line range)\n\
        - `create`: Create a new file (fails if file exists)\n\
        - `str_replace`: Replace an exact unique string in a file\n\
        - `insert`: Insert text at a specific line number".to_string(),
    parameters: json!({
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "enum": ["view", "create", "str_replace", "insert"],
                "description": "The editing command to execute"
            },
            "path": {
                "type": "string",
                "description": "Absolute path to the file or directory"
            },
            "file_text": {
                "type": "string",
                "description": "Required for 'create': the full content of the new file"
            },
            "view_range": {
                "type": "array",
                "items": { "type": "integer" },
                "description": "Optional for 'view': [start_line, end_line] (1-indexed). Use -1 for end_line to mean EOF."
            },
            "old_str": {
                "type": "string",
                "description": "Required for 'str_replace': the exact string to find. Must appear exactly once in the file."
            },
            "new_str": {
                "type": "string",
                "description": "Optional for 'str_replace': replacement string. Omit to delete old_str."
            },
            "insert_line": {
                "type": "integer",
                "description": "Required for 'insert': line number to insert after. 0 = insert at beginning."
            },
            "insert_text": {
                "type": "string",
                "description": "Required for 'insert': the text to insert"
            }
        },
        "required": ["command", "path"]
    }),
}
```

#### Implementation logic (pseudocode for each command)

**`view` command:**
1. Check if path is a directory:
   - If yes, and `view_range` is set, return error.
   - If yes, run `ls -la <path>` or walk directory entries and return listing.
2. If file, read contents with `std::fs::read_to_string`.
3. If `view_range` is provided:
   - Validate it's exactly 2 integers.
   - Validate `start >= 1`, `start <= total_lines`, `end <= total_lines` (or `end == -1`).
   - Slice the lines accordingly.
4. Format output with line numbers (mimicking `cat -n`):
   ```
        1	fn main() {
        2	    println!("hello");
        3	}
   ```
5. Truncate output if > 10,000 characters, append a truncation notice.

**`create` command:**
1. Require `file_text` parameter (return error if missing).
2. Check the file does NOT already exist (return error if it does -- no silent overwrites).
3. Create parent directories if they don't exist (`std::fs::create_dir_all`).
4. Write `file_text` to the path.
5. Return success message.

**`str_replace` command:**
1. Require `old_str` parameter.
2. Read file contents. Normalize tabs with `.replace('\t', "    ")` (4-space tabs).
3. Also normalize `old_str` and `new_str` tabs the same way.
4. Count occurrences of `old_str` in the file content:
   - **0 occurrences**: Return error: `"old_str did not appear verbatim in {path}"`.
   - **> 1 occurrences**: Return error with the line numbers where matches occur: `"Multiple occurrences of old_str in lines [3, 17, 42]. Please provide more surrounding context to make it unique."`.
   - **Exactly 1**: Proceed.
5. Replace `old_str` with `new_str` (default `new_str` to `""` if absent, which means deletion).
6. Write the new content back to the file.
7. Store the old content in an undo history (`HashMap<PathBuf, Vec<String>>`).
8. Return a snippet showing the edited region (4 lines of context before and after the edit location), formatted with line numbers.

**`insert` command:**
1. Require `insert_line` and `insert_text` parameters.
2. Read file, split into lines.
3. Validate `insert_line` is in `[0, total_lines]`.
4. Split the file at `insert_line`: lines before + new text lines + lines after.
5. Write the result back.
6. Store old content in undo history.
7. Return snippet showing the insertion with surrounding context.

#### Struct definition

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Serialize, Deserialize)]
pub struct FileEditTool {
    #[serde(skip)]
    file_history: Mutex<HashMap<PathBuf, Vec<String>>>,
}
```

Use `Mutex` because the Rig `Tool` trait requires `Send + Sync`, and we need interior mutability for the undo history. This is fine since tool calls are not high-contention.

**Important**: Implement `Default` for `FileEditTool` to initialize the `Mutex<HashMap>`. The `#[derive(Serialize, Deserialize)]` is needed because Rig requires it for tools, but `file_history` is skipped.

#### Output type

The output is simply `String` -- formatted text that goes back to the LLM.

#### Error type

```rust
#[derive(Debug, thiserror::Error)]
pub enum FileEditError {
    #[error("{0}")]
    Validation(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
```

---

### 4.2 FileReadTool

**File: `src/tools/file_read.rs`**

A simpler read-only tool. While `file_edit view` can also read files, having a dedicated read tool gives the LLM a clearer/simpler option for "just reading."

#### Args

```rust
#[derive(Deserialize)]
struct FileReadArgs {
    /// Absolute path to the file to read
    path: String,
    /// Optional start line (1-indexed)
    start_line: Option<usize>,
    /// Optional end line (1-indexed, inclusive)
    end_line: Option<usize>,
}
```

#### Tool definition

```rust
ToolDefinition {
    name: "file_read".to_string(),
    description: "Read the contents of a file. Returns the file content with line numbers.".to_string(),
    parameters: json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Absolute path to the file to read"
            },
            "start_line": {
                "type": "integer",
                "description": "Optional: start reading from this line (1-indexed)"
            },
            "end_line": {
                "type": "integer",
                "description": "Optional: stop reading at this line (1-indexed, inclusive)"
            }
        },
        "required": ["path"]
    }),
}
```

#### Implementation

1. Validate path exists and is a file (not directory).
2. Read file with `std::fs::read_to_string`.
3. If `start_line` / `end_line` provided, slice lines.
4. Format with line numbers.
5. Truncate if too long (> 10,000 chars).
6. Return formatted string.

#### Error type

```rust
#[derive(Debug, thiserror::Error)]
pub enum FileReadError {
    #[error("{0}")]
    Validation(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
```

---

### 4.3 BashTool

**File: `src/tools/bash.rs`**

Runs shell commands via `/bin/sh -c`. Essential for the agent to run builds, tests, git commands, grep, etc.

#### Args

```rust
#[derive(Deserialize)]
struct BashArgs {
    /// The shell command to execute
    command: String,
    /// Optional timeout in seconds (default: 30)
    timeout_seconds: Option<u64>,
}
```

#### Tool definition

```rust
ToolDefinition {
    name: "bash".to_string(),
    description: "Run a shell command and return stdout and stderr. \
        Use for running builds, tests, git operations, grep, and other CLI tools. \
        Commands run in /bin/sh. Default timeout is 30 seconds.".to_string(),
    parameters: json!({
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "description": "The shell command to execute"
            },
            "timeout_seconds": {
                "type": "integer",
                "description": "Optional timeout in seconds (default: 30, max: 120)"
            }
        },
        "required": ["command"]
    }),
}
```

#### Implementation

1. Clamp timeout to `[1, 120]` seconds, default 30.
2. Spawn the command with `tokio::process::Command`:
   ```rust
   let child = tokio::process::Command::new("/bin/sh")
       .arg("-c")
       .arg(&args.command)
       .stdout(std::process::Stdio::piped())
       .stderr(std::process::Stdio::piped())
       .spawn()?;
   ```
3. Wait with timeout using `tokio::time::timeout`.
4. On timeout, kill the child process and return an error message.
5. Capture stdout and stderr.
6. Truncate each to 10,000 characters if needed.
7. Return formatted output:
   ```
   Exit code: 0

   STDOUT:
   <stdout content>

   STDERR:
   <stderr content>
   ```

#### Output type

`String` -- the formatted stdout/stderr/exit code.

#### Error type

```rust
#[derive(Debug, thiserror::Error)]
pub enum BashError {
    #[error("Command failed: {0}")]
    Execution(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Command timed out after {0} seconds")]
    Timeout(u64),
}
```

#### Security note

The bash tool is intentionally powerful. The system prompt should instruct the agent to be cautious with destructive commands. We are not sandboxing -- this is a local coding agent the user runs on their own machine.

---

### 4.4 ListDirectoryTool

**File: `src/tools/list_directory.rs`**

Lists files and directories. While bash `ls` could do this, a dedicated tool gives cleaner output.

#### Args

```rust
#[derive(Deserialize)]
struct ListDirectoryArgs {
    /// Absolute path to the directory
    path: String,
    /// Whether to recurse into subdirectories (default false, max depth 3)
    recursive: Option<bool>,
}
```

#### Tool definition

```rust
ToolDefinition {
    name: "list_directory".to_string(),
    description: "List files and directories at the given path. \
        Returns names with indicators for directories (trailing /).".to_string(),
    parameters: json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Absolute path to the directory to list"
            },
            "recursive": {
                "type": "boolean",
                "description": "If true, recurse into subdirectories (max depth 3)"
            }
        },
        "required": ["path"]
    }),
}
```

#### Implementation

1. Validate path exists and is a directory.
2. Use `std::fs::read_dir` to iterate entries.
3. If `recursive` is true, recurse up to depth 3.
4. Sort entries: directories first, then files, alphabetically within each group.
5. Format as a tree or flat listing:
   ```
   src/
   src/main.rs
   src/tools/
   src/tools/mod.rs
   src/tools/bash.rs
   src/tools/file_edit.rs
   src/tools/file_read.rs
   src/tools/list_directory.rs
   Cargo.toml
   ```
6. Skip hidden files/directories (starting with `.`) by default.
7. Truncate output if listing is enormous.

#### Error type

```rust
#[derive(Debug, thiserror::Error)]
pub enum ListDirectoryError {
    #[error("{0}")]
    Validation(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
```

---

## 5. Agent Setup

**In `src/main.rs`:**

```rust
use rig::providers::anthropic;
use rig::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    let client = anthropic::Client::from_env();

    let agent = client
        .agent(anthropic::completion::CLAUDE_4_SONNET)
        .preamble(SYSTEM_PROMPT)
        .max_tokens(4096)
        .tool(FileEditTool::default())
        .tool(FileReadTool)
        .tool(BashTool)
        .tool(ListDirectoryTool)
        .build();

    // Enter REPL loop (see section 6)
    run_repl(agent).await
}
```

### How Rig wires this together

1. `.agent(MODEL)` creates an `AgentBuilder<AnthropicCompletionModel>`.
2. `.preamble(...)` sets the system prompt.
3. `.tool(FileEditTool::default())` transitions to `AgentBuilder<..., WithBuilderTools>` state and registers the tool. Rig calls `Tool::definition()` to get the JSON schema, and `Tool::call()` when the LLM invokes it.
4. Additional `.tool()` calls chain more tools.
5. `.build()` produces an `Agent` that owns a `ToolServerHandle`. The `ToolServer` runs as a background tokio task.
6. When `agent.prompt("...")` is called, Rig enters its agentic loop:
   - Sends the prompt + all tool definitions to Claude.
   - If Claude responds with tool calls, Rig deserializes the args, calls `tool.call(args)`, serializes the output, and sends it back as a `ToolResult`.
   - Repeats until Claude returns a text response.

---

## 6. Interactive REPL Loop

The REPL maintains a chat history across turns so the agent has conversational context.

```rust
use rig::completion::message::Message;
use std::io::{self, BufRead, Write};

async fn run_repl(agent: Agent<impl CompletionModel>) -> anyhow::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut chat_history: Vec<Message> = Vec::new();

    // Print the working directory at startup
    let cwd = std::env::current_dir()?;
    println!("PeakBot coding agent ready.");
    println!("Working directory: {}", cwd.display());
    println!("Type your message (or 'exit' to quit).\n");

    loop {
        print!("> ");
        stdout.flush()?;

        let mut input = String::new();
        stdin.lock().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            continue;
        }
        if input == "exit" || input == "quit" {
            println!("Goodbye!");
            break;
        }

        // Use .prompt() with history for multi-turn conversation
        match agent
            .prompt(input)
            .with_history(&mut chat_history)
            .max_turns(15)
            .await
        {
            Ok(response) => {
                println!("\n{}\n", response);
            }
            Err(e) => {
                eprintln!("\nError: {}\n", e);
            }
        }
    }

    Ok(())
}
```

### Key details

- **`with_history(&mut chat_history)`**: Rig mutably borrows the history vec. After each `.prompt()` call, Rig appends the user message, all assistant/tool messages, and the final response to this vec. On the next call, Rig sends the full history to Claude, giving it conversational memory.
- **`max_turns(15)`**: Limits the agentic loop to 15 tool-use rounds per user message. This prevents runaway loops.
- The `Agent` type from Rig is generic over `CompletionModel`. Use `impl CompletionModel` for the function signature or store it as a concrete type.

---

## 7. Error Handling Strategy

### Tool-level errors

Each tool has its own error enum implementing `std::error::Error`. When a tool returns `Err(...)`, Rig catches it, converts it to a `ToolError`, and sends the error message back to Claude as a tool result. Claude then typically adjusts its approach (e.g., fixes the path, changes the `old_str`).

This is the desired behavior -- tool errors should NOT crash the agent. They should inform the LLM so it can self-correct.

### Agent-level errors

`agent.prompt()` returns `Result<String, PromptError>`. Possible errors:
- `PromptError::CompletionError` -- API call failure (network, auth, etc.)
- `PromptError::MaxTurnsError` -- hit the max_turns limit without a text response

In the REPL, we print these and continue the loop.

### Validation pattern

Every tool should validate inputs eagerly and return descriptive error messages. The error messages should tell the LLM what went wrong and hint at how to fix it. Examples:

- `"The path /foo/bar is not absolute. It should start with '/'."`
- `"File /foo/bar.rs does not exist. Use the 'list_directory' tool to check available files."`
- `"old_str was not found verbatim in /foo/bar.rs. Make sure you're copying the exact text including whitespace."`
- `"old_str appeared 3 times in /foo/bar.rs at lines [5, 12, 28]. Include more surrounding context to make the match unique."`

---

## 8. File-by-File Implementation Guide

### 8.1 `Cargo.toml`

See [Section 2](#2-dependencies) for the exact contents. Overwrite the existing file.

### 8.2 `src/main.rs`

```rust
mod tools;

use anyhow::Result;
use rig::completion::message::Message;
use rig::prelude::*;
use rig::providers::anthropic;
use std::io::{self, BufRead, Write};

use tools::{BashTool, FileEditTool, FileReadTool, ListDirectoryTool};

const SYSTEM_PROMPT: &str = include_str!("system_prompt.txt");

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    let client = anthropic::Client::from_env();

    let agent = client
        .agent(anthropic::completion::CLAUDE_4_SONNET)
        .preamble(SYSTEM_PROMPT)
        .max_tokens(4096)
        .tool(FileEditTool::default())
        .tool(FileReadTool)
        .tool(BashTool)
        .tool(ListDirectoryTool)
        .build();

    let cwd = std::env::current_dir()?;
    println!("PeakBot coding agent ready.");
    println!("Working directory: {}", cwd.display());
    println!("Type your message (or 'exit' to quit).\n");

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut chat_history: Vec<Message> = Vec::new();

    loop {
        print!("> ");
        stdout.flush()?;

        let mut input = String::new();
        stdin.lock().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            continue;
        }
        if input.eq_ignore_ascii_case("exit") || input.eq_ignore_ascii_case("quit") {
            println!("Goodbye!");
            break;
        }

        match agent
            .prompt(input)
            .with_history(&mut chat_history)
            .max_turns(15)
            .await
        {
            Ok(response) => {
                println!("\n{}\n", response);
            }
            Err(e) => {
                eprintln!("\nError: {}\n", e);
            }
        }
    }

    Ok(())
}
```

**Notes:**
- `include_str!("system_prompt.txt")` loads the system prompt from a separate file at compile time. This keeps `main.rs` clean.
- The agent type is inferred by the compiler. No need to name it explicitly.
- `chat_history` persists across loop iterations, giving the agent multi-turn memory.

### 8.3 `src/system_prompt.txt`

See [Section 9](#9-system-prompt) for the full text. Create this file at `src/system_prompt.txt`.

### 8.4 `src/tools/mod.rs`

```rust
mod bash;
mod file_edit;
mod file_read;
mod list_directory;

pub use bash::BashTool;
pub use file_edit::FileEditTool;
pub use file_read::FileReadTool;
pub use list_directory::ListDirectoryTool;
```

### 8.5 `src/tools/file_edit.rs`

This is the largest file (~250-300 lines). Structure:

```rust
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// ── Constants ──────────────────────────────────────────
const SNIPPET_CONTEXT_LINES: usize = 4;
const MAX_OUTPUT_CHARS: usize = 10_000;
const TRUNCATION_NOTICE: &str = "\n... [output truncated] Use file_read with start_line/end_line or bash with grep -n to find specific content.";

// ── Error ──────────────────────────────────────────────
#[derive(Debug, thiserror::Error)]
pub enum FileEditError {
    #[error("{0}")]
    Validation(String),
    #[error("IO error on {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

// ── Args ───────────────────────────────────────────────
#[derive(Deserialize)]
pub struct FileEditArgs {
    command: String,
    path: String,
    file_text: Option<String>,
    view_range: Option<Vec<i64>>,
    old_str: Option<String>,
    new_str: Option<String>,
    insert_line: Option<usize>,
    insert_text: Option<String>,
}

// ── Tool struct ────────────────────────────────────────
#[derive(Serialize, Deserialize)]
pub struct FileEditTool {
    /// Skipped in serde -- runtime-only undo history
    #[serde(skip)]
    file_history: Mutex<HashMap<PathBuf, Vec<String>>>,
}

impl Default for FileEditTool {
    fn default() -> Self {
        Self {
            file_history: Mutex::new(HashMap::new()),
        }
    }
}

impl Tool for FileEditTool {
    const NAME: &'static str = "file_edit";
    type Error = FileEditError;
    type Args = FileEditArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        // Return the ToolDefinition with the JSON schema from section 4.1
        todo!()
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        match args.command.as_str() {
            "view" => self.cmd_view(&args),
            "create" => self.cmd_create(&args),
            "str_replace" => self.cmd_str_replace(&args),
            "insert" => self.cmd_insert(&args),
            other => Err(FileEditError::Validation(format!(
                "Unknown command '{}'. Valid commands: view, create, str_replace, insert",
                other
            ))),
        }
    }
}

impl FileEditTool {
    // ── View ───────────────────────────────────────────
    fn cmd_view(&self, args: &FileEditArgs) -> Result<String, FileEditError> {
        let path = Path::new(&args.path);
        self.validate_path_exists(path)?;

        if path.is_dir() {
            if args.view_range.is_some() {
                return Err(FileEditError::Validation(
                    "view_range is not allowed for directories".into(),
                ));
            }
            // List directory contents (non-recursive, up to 2 levels)
            return self.list_dir_contents(path);
        }

        let content = self.read_file(path)?;
        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();

        let (start, end) = match &args.view_range {
            Some(range) => {
                // Validate range is [start, end] with exactly 2 elements
                // Handle -1 sentinel for "to end of file"
                // Return 0-indexed (start_idx, end_idx)
                self.parse_view_range(range, total)?
            }
            None => (0, total),
        };

        let selected: Vec<String> = lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>6}\t{}", start + i + 1, line))
            .collect();

        let output = selected.join("\n");
        Ok(maybe_truncate(&output))
    }

    // ── Create ─────────────────────────────────────────
    fn cmd_create(&self, args: &FileEditArgs) -> Result<String, FileEditError> {
        let path = Path::new(&args.path);
        let file_text = args.file_text.as_deref().ok_or_else(|| {
            FileEditError::Validation("'file_text' is required for create command".into())
        })?;

        if path.exists() {
            return Err(FileEditError::Validation(format!(
                "File already exists at {}. Cannot overwrite with 'create'. Use 'str_replace' to edit existing files.",
                path.display()
            )));
        }

        // Create parent dirs if needed
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| FileEditError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }

        self.write_file(path, file_text)?;
        Ok(format!("File created successfully at: {}", path.display()))
    }

    // ── str_replace ────────────────────────────────────
    fn cmd_str_replace(&self, args: &FileEditArgs) -> Result<String, FileEditError> {
        let path = Path::new(&args.path);
        self.validate_path_exists(path)?;

        let old_str = args.old_str.as_deref().ok_or_else(|| {
            FileEditError::Validation("'old_str' is required for str_replace command".into())
        })?;
        let new_str = args.new_str.as_deref().unwrap_or("");

        let content = self.read_file(path)?;

        // Count occurrences
        let count = content.matches(old_str).count();
        if count == 0 {
            return Err(FileEditError::Validation(format!(
                "old_str not found verbatim in {}. Ensure you're matching the exact text including whitespace and indentation.",
                path.display()
            )));
        }
        if count > 1 {
            // Find which lines contain the match to help the LLM
            let line_nums: Vec<usize> = content
                .lines()
                .enumerate()
                .filter(|(_, line)| line.contains(old_str))
                .map(|(i, _)| i + 1)
                .collect();
            return Err(FileEditError::Validation(format!(
                "old_str appears {} times in {} at lines {:?}. Include more surrounding context to make the match unique.",
                count, path.display(), line_nums
            )));
        }

        // Save undo history
        self.push_history(path, &content);

        // Perform replacement
        let new_content = content.replacen(old_str, new_str, 1);
        self.write_file(path, &new_content)?;

        // Build context snippet
        let replacement_line = content.split(old_str).next().unwrap_or("").matches('\n').count();
        let new_lines: Vec<&str> = new_content.lines().collect();
        let start = replacement_line.saturating_sub(SNIPPET_CONTEXT_LINES);
        let end = (replacement_line + SNIPPET_CONTEXT_LINES + new_str.matches('\n').count() + 1)
            .min(new_lines.len());
        let snippet: String = new_lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>6}\t{}", start + i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(format!(
            "File {} has been edited. Here's the result around the edit:\n{}\nReview the changes and edit again if necessary.",
            path.display(),
            snippet
        ))
    }

    // ── Insert ─────────────────────────────────────────
    fn cmd_insert(&self, args: &FileEditArgs) -> Result<String, FileEditError> {
        let path = Path::new(&args.path);
        self.validate_path_exists(path)?;

        let insert_line = args.insert_line.ok_or_else(|| {
            FileEditError::Validation("'insert_line' is required for insert command".into())
        })?;
        let insert_text = args.insert_text.as_deref().ok_or_else(|| {
            FileEditError::Validation("'insert_text' is required for insert command".into())
        })?;

        let content = self.read_file(path)?;
        let mut lines: Vec<&str> = content.lines().collect();
        let total = lines.len();

        if insert_line > total {
            return Err(FileEditError::Validation(format!(
                "insert_line {} is out of range. File has {} lines. Valid range: [0, {}].",
                insert_line, total, total
            )));
        }

        // Save undo history
        self.push_history(path, &content);

        // Insert new lines
        let new_lines: Vec<&str> = insert_text.lines().collect();
        let mut result_lines = Vec::with_capacity(total + new_lines.len());
        result_lines.extend_from_slice(&lines[..insert_line]);
        result_lines.extend_from_slice(&new_lines);
        result_lines.extend_from_slice(&lines[insert_line..]);

        let new_content = result_lines.join("\n");
        self.write_file(path, &new_content)?;

        // Build context snippet
        let start = insert_line.saturating_sub(SNIPPET_CONTEXT_LINES);
        let end = (insert_line + new_lines.len() + SNIPPET_CONTEXT_LINES).min(result_lines.len());
        let snippet: String = result_lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>6}\t{}", start + i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(format!(
            "File {} has been edited. Here's the result around the insertion:\n{}\nReview the changes and edit again if necessary.",
            path.display(),
            snippet
        ))
    }

    // ── Helpers ────────────────────────────────────────

    fn validate_path_exists(&self, path: &Path) -> Result<(), FileEditError> {
        if !path.is_absolute() {
            return Err(FileEditError::Validation(format!(
                "Path '{}' is not absolute. Use an absolute path starting with '/'.",
                path.display()
            )));
        }
        if !path.exists() {
            return Err(FileEditError::Validation(format!(
                "Path '{}' does not exist.",
                path.display()
            )));
        }
        Ok(())
    }

    fn read_file(&self, path: &Path) -> Result<String, FileEditError> {
        std::fs::read_to_string(path).map_err(|e| FileEditError::Io {
            path: path.to_path_buf(),
            source: e,
        })
    }

    fn write_file(&self, path: &Path, content: &str) -> Result<(), FileEditError> {
        std::fs::write(path, content).map_err(|e| FileEditError::Io {
            path: path.to_path_buf(),
            source: e,
        })
    }

    fn push_history(&self, path: &Path, content: &str) {
        let mut history = self.file_history.lock().unwrap();
        history
            .entry(path.to_path_buf())
            .or_default()
            .push(content.to_string());
    }

    fn parse_view_range(&self, range: &[i64], total: usize) -> Result<(usize, usize), FileEditError> {
        if range.len() != 2 {
            return Err(FileEditError::Validation(
                "view_range must be exactly [start_line, end_line]".into(),
            ));
        }
        let start = range[0];
        let end = range[1];

        if start < 1 || start as usize > total {
            return Err(FileEditError::Validation(format!(
                "view_range start {} is out of range [1, {}]",
                start, total
            )));
        }
        let start_idx = (start - 1) as usize;

        let end_idx = if end == -1 {
            total
        } else {
            if (end as usize) > total {
                return Err(FileEditError::Validation(format!(
                    "view_range end {} exceeds file length {}",
                    end, total
                )));
            }
            if end < start {
                return Err(FileEditError::Validation(format!(
                    "view_range end {} is less than start {}",
                    end, start
                )));
            }
            end as usize
        };

        Ok((start_idx, end_idx))
    }

    fn list_dir_contents(&self, path: &Path) -> Result<String, FileEditError> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(path).map_err(|e| FileEditError::Io {
            path: path.to_path_buf(),
            source: e,
        })? {
            let entry = entry.map_err(|e| FileEditError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue; // skip hidden
            }
            let suffix = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                "/"
            } else {
                ""
            };
            entries.push(format!("{}{}", name, suffix));
        }
        entries.sort();
        Ok(format!(
            "Directory listing of {}:\n{}",
            path.display(),
            entries.join("\n")
        ))
    }
}

fn maybe_truncate(s: &str) -> String {
    if s.len() > MAX_OUTPUT_CHARS {
        format!("{}{}", &s[..MAX_OUTPUT_CHARS], TRUNCATION_NOTICE)
    } else {
        s.to_string()
    }
}
```

**Implementation notes:**
- The `Mutex` for `file_history` is fine here. Tool calls are sequential per-agent-turn (or concurrent but each on separate files). Lock contention is negligible.
- The `#[derive(Serialize, Deserialize)]` on `FileEditTool` is required because Rig uses it internally. The `#[serde(skip)]` on `file_history` means it serializes as the default (empty map).
- All string matching is exact (no regex). This mirrors Claude's edit tool behavior.

### 8.6 `src/tools/file_read.rs`

```rust
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;

const MAX_OUTPUT_CHARS: usize = 10_000;
const TRUNCATION_NOTICE: &str = "\n... [output truncated] Use start_line/end_line to read specific sections.";

#[derive(Debug, thiserror::Error)]
pub enum FileReadError {
    #[error("{0}")]
    Validation(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Deserialize)]
pub struct FileReadArgs {
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

#[derive(Serialize, Deserialize)]
pub struct FileReadTool;

impl Tool for FileReadTool {
    const NAME: &'static str = "file_read";
    type Error = FileReadError;
    type Args = FileReadArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        // JSON schema from section 4.2
        todo!()
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let path = Path::new(&args.path);

        if !path.is_absolute() {
            return Err(FileReadError::Validation(format!(
                "Path '{}' is not absolute.",
                args.path
            )));
        }
        if !path.exists() {
            return Err(FileReadError::Validation(format!(
                "File '{}' does not exist.",
                args.path
            )));
        }
        if path.is_dir() {
            return Err(FileReadError::Validation(format!(
                "'{}' is a directory. Use list_directory instead.",
                args.path
            )));
        }

        let content = std::fs::read_to_string(path)?;
        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();

        let start = args.start_line.map(|s| s.saturating_sub(1)).unwrap_or(0);
        let end = args.end_line.unwrap_or(total).min(total);

        if start >= total {
            return Err(FileReadError::Validation(format!(
                "start_line {} exceeds file length of {} lines",
                start + 1,
                total
            )));
        }

        let output: String = lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>6}\t{}", start + i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(maybe_truncate(&output))
    }
}

fn maybe_truncate(s: &str) -> String {
    if s.len() > MAX_OUTPUT_CHARS {
        format!("{}{}", &s[..MAX_OUTPUT_CHARS], TRUNCATION_NOTICE)
    } else {
        s.to_string()
    }
}
```

### 8.7 `src/tools/bash.rs`

```rust
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use tokio::process::Command;

const MAX_OUTPUT_CHARS: usize = 10_000;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, thiserror::Error)]
pub enum BashError {
    #[error("Failed to execute command: {0}")]
    Execution(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Deserialize)]
pub struct BashArgs {
    command: String,
    timeout_seconds: Option<u64>,
}

#[derive(Serialize, Deserialize)]
pub struct BashTool;

impl Tool for BashTool {
    const NAME: &'static str = "bash";
    type Error = BashError;
    type Args = BashArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        // JSON schema from section 4.3
        todo!()
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let timeout_secs = args
            .timeout_seconds
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS);

        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(&args.command)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| BashError::Execution(format!("Failed to spawn shell: {}", e)))?;

        let result = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            child.wait_with_output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let exit_code = output.status.code().unwrap_or(-1);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                let stdout = maybe_truncate(&stdout);
                let stderr = maybe_truncate(&stderr);

                let mut result = format!("Exit code: {}\n", exit_code);
                if !stdout.is_empty() {
                    result.push_str(&format!("\nSTDOUT:\n{}\n", stdout));
                }
                if !stderr.is_empty() {
                    result.push_str(&format!("\nSTDERR:\n{}\n", stderr));
                }
                Ok(result)
            }
            Ok(Err(e)) => Err(BashError::Execution(format!("Command failed: {}", e))),
            Err(_) => {
                // Timeout -- kill the process
                let _ = child.kill().await;
                Err(BashError::Execution(format!(
                    "Command timed out after {} seconds. Consider increasing timeout_seconds.",
                    timeout_secs
                )))
            }
        }
    }
}

fn maybe_truncate(s: &str) -> String {
    if s.len() > MAX_OUTPUT_CHARS {
        format!("{}... [truncated, {} total chars]", &s[..MAX_OUTPUT_CHARS], s.len())
    } else {
        s.to_string()
    }
}
```

**Note on `child.wait_with_output()`**: After spawning, we use `tokio::time::timeout` wrapping `child.wait_with_output()`. If the timeout fires, we need to kill the child. The `child` variable must still be in scope. The implementation above handles this by NOT moving `child` into `wait_with_output` -- instead, use the pattern of `child.wait_with_output()` which consumes the child. For the timeout case, you'll need a different approach:

**Corrected pattern:**
```rust
// Spawn the child
let mut child = Command::new("/bin/sh")
    .arg("-c")
    .arg(&args.command)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()?;

// Take stdout/stderr handles before waiting
let stdout_handle = child.stdout.take();
let stderr_handle = child.stderr.take();

// Wait with timeout
match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait()).await {
    Ok(Ok(status)) => {
        // Read stdout/stderr from handles
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        if let Some(mut h) = stdout_handle {
            tokio::io::AsyncReadExt::read_to_end(&mut h, &mut stdout_buf).await?;
        }
        if let Some(mut h) = stderr_handle {
            tokio::io::AsyncReadExt::read_to_end(&mut h, &mut stderr_buf).await?;
        }
        // Format output...
    }
    Ok(Err(e)) => { /* process error */ }
    Err(_) => {
        // Timeout -- kill
        let _ = child.kill().await;
        // Return timeout error
    }
}
```

Alternatively, keep it simpler with `wait_with_output()` wrapped in `tokio::time::timeout` and use `child.start_kill()` in the error path (since `wait_with_output` consumes child, you'd need to restructure). **The simplest correct approach is:**

```rust
let child = Command::new("/bin/sh")
    .arg("-c")
    .arg(&args.command)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)  // <-- auto-kill if dropped
    .spawn()?;

match tokio::time::timeout(
    Duration::from_secs(timeout_secs),
    child.wait_with_output(),
).await {
    Ok(Ok(output)) => { /* success path */ }
    Ok(Err(e)) => { /* execution error */ }
    Err(_) => {
        // child is dropped here -> killed automatically due to kill_on_drop
        Err(BashError::Execution(format!("Command timed out after {} seconds", timeout_secs)))
    }
}
```

Use `.kill_on_drop(true)` on the `Command` builder. This is the cleanest solution.

### 8.8 `src/tools/list_directory.rs`

```rust
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ListDirectoryError {
    #[error("{0}")]
    Validation(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Deserialize)]
pub struct ListDirectoryArgs {
    path: String,
    recursive: Option<bool>,
}

#[derive(Serialize, Deserialize)]
pub struct ListDirectoryTool;

impl Tool for ListDirectoryTool {
    const NAME: &'static str = "list_directory";
    type Error = ListDirectoryError;
    type Args = ListDirectoryArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        // JSON schema from section 4.4
        todo!()
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let path = Path::new(&args.path);

        if !path.is_absolute() {
            return Err(ListDirectoryError::Validation(format!(
                "Path '{}' is not absolute.",
                args.path
            )));
        }
        if !path.exists() {
            return Err(ListDirectoryError::Validation(format!(
                "Path '{}' does not exist.",
                args.path
            )));
        }
        if !path.is_dir() {
            return Err(ListDirectoryError::Validation(format!(
                "'{}' is not a directory. Use file_read to read files.",
                args.path
            )));
        }

        let recursive = args.recursive.unwrap_or(false);
        let max_depth = if recursive { 3 } else { 1 };

        let mut entries = Vec::new();
        self.collect_entries(path, path, 0, max_depth, &mut entries)?;
        entries.sort();

        Ok(entries.join("\n"))
    }
}

impl ListDirectoryTool {
    fn collect_entries(
        &self,
        base: &Path,
        dir: &Path,
        depth: usize,
        max_depth: usize,
        entries: &mut Vec<String>,
    ) -> Result<(), ListDirectoryError> {
        if depth >= max_depth {
            return Ok(());
        }

        let mut dir_entries: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| ListDirectoryError::Io(e))?
            .filter_map(|e| e.ok())
            .collect();

        dir_entries.sort_by_key(|e| e.file_name());

        for entry in dir_entries {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }

            let rel_path = entry
                .path()
                .strip_prefix(base)
                .unwrap_or(&entry.path())
                .to_string_lossy()
                .to_string();

            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                entries.push(format!("{}/", rel_path));
                self.collect_entries(base, &entry.path(), depth + 1, max_depth, entries)?;
            } else {
                entries.push(rel_path);
            }
        }

        Ok(())
    }
}
```

---

## 9. System Prompt

Create `src/system_prompt.txt`:

```text
You are PeakBot, a coding agent that helps users with software engineering tasks. You operate in the user's local filesystem and can read, write, and execute code.

## Available Tools

You have four tools:

1. **file_edit** - Edit files with four commands:
   - `view`: Read file contents (with optional line range) or list a directory
   - `create`: Create a new file (fails if file already exists)
   - `str_replace`: Replace an exact, unique string in a file
   - `insert`: Insert text at a specific line number

2. **file_read** - Read file contents with optional line range. Simpler than file_edit view.

3. **bash** - Execute shell commands. Use for building, testing, git, grep, and other CLI operations.

4. **list_directory** - List files and directories at a path, optionally recursive.

## Working Principles

- Always read a file before editing it. Understand existing code before modifying it.
- Use absolute paths for all file operations.
- When using `str_replace`, include enough surrounding context in `old_str` to make it unique. If your replacement fails because the string appears multiple times, add more lines of context.
- After making changes, verify them by reading the modified region or running the build/tests.
- Keep changes minimal and focused. Only modify what's needed.
- If a command fails, read the error message carefully and adjust your approach.
- Use `bash` with `grep -rn` to search for patterns across files.
- Use `list_directory` to explore project structure before diving into files.

## Safety

- Be cautious with destructive bash commands (rm -rf, etc.)
- Never overwrite files without reading them first
- Prefer targeted edits (str_replace) over rewriting entire files
```

---

## 10. Testing

### Manual testing checklist

After building (`cargo build`), test each tool:

1. **list_directory**: Ask "List the files in this project"
2. **file_read**: Ask "Read the contents of Cargo.toml"
3. **file_edit create**: Ask "Create a file at /tmp/peakbot_test.txt with the content 'hello world'"
4. **file_edit str_replace**: Ask "Change 'hello world' to 'hello peakbot' in /tmp/peakbot_test.txt"
5. **file_edit insert**: Ask "Add a second line saying 'line 2' to /tmp/peakbot_test.txt"
6. **bash**: Ask "Run `cargo check` in this project"
7. **Multi-turn**: Ask "Add a new function `add(a: i32, b: i32) -> i32` to src/main.rs and make main call it"

### Build and run

```bash
export ANTHROPIC_API_KEY="your-key-here"
cargo run
```

### Potential issues to watch for

- **Rig version compatibility**: The API may have minor differences between 0.31.x releases. If `with_history` signature changes, check the Rig docs.
- **Serde derive on tool structs**: Rig requires `Serialize + Deserialize` on tool structs. Make sure the `#[derive]` is present even on unit structs like `pub struct BashTool;`.
- **Async in tool::call()**: The `call` method is async but the `FileEditTool` methods above are sync (no `.await`). This is fine -- sync functions called from async context work. But for `BashTool`, the `call` method genuinely needs to be async for `tokio::process::Command`.
- **`Mutex` poisoning**: If a tool panics while holding the mutex, subsequent calls will fail. Use `.lock().unwrap()` for simplicity (panics are bugs we want to catch), or `.lock().unwrap_or_else(|e| e.into_inner())` for resilience.
- **Line ending normalization**: The current implementation uses `\n` joins. Files with `\r\n` (Windows) line endings may not round-trip perfectly. For a simple agent this is acceptable.

---

## Summary

| File | Lines (est.) | Purpose |
|------|-------------|---------|
| `Cargo.toml` | 15 | Dependencies |
| `src/main.rs` | 65 | Client setup, agent build, REPL |
| `src/system_prompt.txt` | 35 | System prompt for Claude |
| `src/tools/mod.rs` | 10 | Module re-exports |
| `src/tools/file_edit.rs` | 280 | File editing (view/create/str_replace/insert) |
| `src/tools/file_read.rs` | 75 | File reading |
| `src/tools/bash.rs` | 90 | Shell command execution |
| `src/tools/list_directory.rs` | 95 | Directory listing |
| **Total** | **~665** | |

The implementation is straightforward: define 4 tool structs, implement `Tool` for each, wire them into a Rig agent, and run a REPL. Rig handles all the complexity of the agentic loop, tool dispatch, and API communication.
