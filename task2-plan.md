# Task 2 Implementation Plan: Think Tool

## Overview

Implement the "think" tool as described in todo.md to give Claude dedicated space for structured thinking during complex tool use situations.

## Implementation Steps

### Step 1: Create the Think Tool (`src/tools/think.rs`)

**Objective**: Create a new tool that echoes back thoughts with a prefix.

**File to create**: `/var/home/exe/workz/peakbot/src/tools/think.rs`

**Implementation pattern** (following `file_read.rs`):

```rust
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ThinkError {
    #[error("{0}")]
    Validation(String),
}

#[derive(Deserialize)]
pub struct ThinkArgs {
    thought: String,
}

#[derive(Serialize, Deserialize)]
pub struct ThinkTool;

impl Tool for ThinkTool {
    const NAME: &'static str = "think";
    type Error = ThinkError;
    type Args = ThinkArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "think".to_string(),
            description: "Use it when complex reasoning or brainstorming is needed.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "thought": {
                        "type": "string",
                        "description": "Your thoughts. Use it when complex reasoning or brainstorming is needed. For example, if you explore the repo and discover the source of a bug, call this tool to brainstorm several unique ways of fixing the bug, and assess which change(s) are likely to be simplest and most effective."
                    }
                },
                "required": ["thought"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Log before execution
        tracing::info!(
            target: "peakbot",
            tool_type = "think",
            thought_length = args.thought.len(),
            "Think tool executed"
        );

        Ok(format!("Thinking: {}", args.thought))
    }
}
```

**Estimated effort**: Low (simple tool, ~30 lines)

---

### Step 2: Export ThinkTool in `src/tools/mod.rs`

**Objective**: Make the ThinkTool publicly accessible.

**File to modify**: `/var/home/exe/workz/peakbot/src/tools/mod.rs`

**Changes**:
1. Add `mod think;` after other module declarations
2. Add `pub use think::ThinkTool;` after other exports

**Changes detail**:
```rust
// Add module declaration
mod think;

// Add export (new line after line 8)
pub use think::ThinkTool;
```

**Estimated effort**: Low (2 lines)

---

### Step 3: Add ThinkTool to Agent in `src/lib.rs`

**Objective**: Make the think tool available to the model alongside other built-in tools.

**File to modify**: `/var/home/exe/workz/peakbot/src/lib.rs`

**Changes**:

1. **Add ThinkTool to the public exports** (line 15-17):
   - Add `ThinkTool` to the `pub use tools::` list

2. **Add the tool to the agent builder** (line 110-115):
   - Add `.tool(ThinkTool)` after other tool additions

**Changes detail**:

In `pub use tools::{...}` (line 15-17):
```rust
pub use tools::{
    BashTool, FetchUrlTool, FileEditTool, FileReadTool, ListDirectoryTool, LoggingToolDyn,
    ThinkTool,  // Add this
};
```

In `build_agent()` function (around line 110):
```rust
.tool(FileEditTool::default())
.tool(FileReadTool)
.tool(BashTool)
.tool(ListDirectoryTool)
.tool(FetchUrlTool)
.tool(ThinkTool)  // Add this line
.tools(mcp_tools)
```

**Estimated effort**: Low (4 lines total)

---

### Step 4: Update System Prompt (`src/system_prompt.txt`)

**Objective**: Add documentation explaining when to use the think tool.

**File to modify**: `/var/home/exe/workz/peakbot/src/system_prompt.txt`

**Changes**: Append the following section after the existing content:

```
# Using the think tool

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

**Estimated effort**: Low (15 lines)

---

### Step 5: Verify and Test

**Objective**: Ensure the implementation compiles and works correctly.

**Actions**:
1. Run `cargo build` to verify compilation
2. Verify the think tool appears in tool definitions (could add debug output)

**Expected result**: Build succeeds without errors.

**Estimated effort**: Low

---

## Dependencies

- **External**: None (uses existing rig::tool::Tool trait)
- **Internal**: None (ThinkTool is self-contained)

## Files Modified

| File | Change Type | Lines |
|------|-------------|-------|
| `src/tools/think.rs` | New file | ~45 |
| `src/tools/mod.rs` | Modify | +2 |
| `src/lib.rs` | Modify | +4 |
| `src/system_prompt.txt` | Modify | +15 |

## Total Estimated Effort

- **Time**: ~30 minutes
- **Complexity**: Low (simple tool, follows existing patterns)

## Validation Checklist

- [x] `src/tools/think.rs` created with proper Tool implementation
- [x] `ThinkTool` exported from `src/tools/mod.rs`
- [x] `ThinkTool` added to agent in `src/lib.rs`
- [x] System prompt updated with think tool documentation
- [x] Code compiles successfully (`cargo build`)