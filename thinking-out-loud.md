# Thinking Out Loud: Streaming Agent Output Plan

## Goal
Print the agent's thinking and messages in real-time as it works, not just the final output.

## Current Architecture Analysis

### Event Flow
```
SessionHook (emits events)
    ↓ (via mpsc channel)
EventProcessor (consumes events)
    ↓ (dispatches to handlers)
Handlers (CostHandler, ConversationHandler)
```

### Available Events (from `events.rs`)
1. **`CompletionRequest`** - Before LLM call (message count, estimated tokens)
2. **`CompletionResponse`** - After LLM response (content, reasoning, usage)
3. **`ToolCall`** - When agent calls a tool (tool_name, arguments)
4. **`ToolResult`** - After tool executes (result, success)
5. **`SessionStart` / `SessionEnd`** - Session lifecycle

### Key Insight
The `CompletionResponse` event already contains:
- `content: String` - The agent's text response
- `reasoning: Option<String>` - The agent's thinking (if model supports it)

## Implementation Plan

### 1. Create a New Handler: `StreamingOutputHandler`

**Location**: `src/hooks/streaming_output_handler.rs`

**Purpose**: Print agent messages and thinking in real-time with nice formatting.

**Events to Handle**:
- `CompletionRequest` - Print "Agent is thinking..." indicator
- `CompletionResponse` - Print reasoning (if present) and content
- `ToolCall` - Print "Calling tool: {name}..." 
- `ToolResult` - Print tool result summary (optional, maybe just on error)

**Features**:
- Color-coded output (using `colored` crate or ANSI codes)
- Thinking blocks with distinct styling
- Tool call indicators
- Configurable verbosity levels

### 2. Handler Implementation Details

```rust
pub struct StreamingOutputHandler {
    verbosity: VerbosityLevel,  // Quiet, Normal, Verbose
    show_tool_results: bool,
    show_thinking: bool,
}

enum VerbosityLevel {
    Quiet,    // Only final output
    Normal,   // Thinking + output + tool calls
    Verbose,  // Everything including tool results
}
```

**Output Format Example**:
```
🤔 Agent is thinking...

💭 Thinking:
Let me first check what files are in the directory...

🔧 Calling tool: list_directory
   path: /var/home/exe/workz/peakhands/peakbot/src

✅ Tool completed: list_directory

💭 Thinking:
Now I can see the structure. I'll read the hooks module...

🔧 Calling tool: file_read
   path: /var/home/exe/workz/peakhands/peakbot/src/hooks/mod.rs

✅ Tool completed: file_read

Here's what I found...
```

### 3. Integration Points

#### A. Add to `src/hooks/mod.rs`
```rust
pub mod streaming_output_handler;
pub use streaming_output_handler::StreamingOutputHandler;
```

#### B. Add to `src/lib.rs` (AgentRunner)
In the event processor setup (around line 440-455):

```rust
// Add streaming output handler
let streaming_handler = StreamingOutputHandler::new(
    VerbosityLevel::Normal,
    true,  // show_tool_results
    true,  // show_thinking
);
handlers.push(Arc::new(streaming_handler));
```

#### C. Optional: Configuration Support
Add to `config.yaml`:
```yaml
streaming_output:
  enabled: true
  verbosity: "normal"  # quiet, normal, verbose
  show_thinking: true
  show_tool_calls: true
  show_tool_results: false
```

### 4. File Structure

```
src/hooks/
├── streaming_output_handler.rs  [NEW]
├── mod.rs                       [UPDATE]
└── ... (existing files)
```

### 5. Implementation Steps

**Step 1**: Create `streaming_output_handler.rs`
- Define `StreamingOutputHandler` struct
- Define `VerbosityLevel` enum
- Implement `EventHandler` trait
- Add nice formatting with emojis/ANSI colors

**Step 2**: Update `mod.rs`
- Add module declaration
- Re-export the new handler

**Step 3**: Update `lib.rs`
- Import the handler
- Add it to the handlers list in `run()` method

**Step 4**: (Optional) Add configuration support
- Add config struct fields
- Read from config.yaml
- Make handler configurable at runtime

**Step 5**: Test and refine
- Test with different models
- Adjust formatting
- Add/reduce verbosity options

### 6. Dependencies

May need to add to `Cargo.toml`:
```toml
[dependencies]
colored = "2.1"  # For colored terminal output (optional)
```

Or use built-in ANSI codes to avoid dependency:
```rust
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";
```

### 7. Edge Cases to Handle

1. **Thread safety**: Handler is called from async task, use `println!` safely
2. **Formatting**: Long tool arguments/results might need truncation
3. **Race conditions**: Multiple rapid events - consider buffering or rate limiting
4. **Terminal support**: Check if stdout is a TTY before using colors

### 8. Future Enhancements

- **Progress bars** for long operations
- **Collapsible thinking** blocks (show summary, expand on demand)
- **Logging to file** in addition to stdout
- **JSON output mode** for programmatic consumption
- **Rich text formatting** with terminals that support it

## Summary

The plan is straightforward:
1. Create a new `StreamingOutputHandler` that implements `EventHandler`
2. Handle `CompletionResponse` events to print thinking and messages
3. Handle `ToolCall`/`ToolResult` events to show tool usage
4. Register it alongside existing handlers in `AgentRunner`
5. Optionally add configuration for verbosity control

This leverages the existing event-driven architecture cleanly without modifying the core agent logic.
