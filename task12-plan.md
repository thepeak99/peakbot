# Task 12 Plan: Todo List Tool

## Status: ⏳ PLANNED

**Implementation Date:** To be determined

## Overview

Implement a todo list tool that allows the model to track its own progress on multi-step tasks. The model can create tasks, update their status, and view the current state of its work plan.

## Why This Is Important

- Helps the model organize and track multi-step tasks
- Keeps users informed about progress
- Enables the model to show its plan before executing complex operations
- Provides transparency in the model's reasoning process

---

## Implementation Plan

### Phase 1: Create Todo Data Structures

**1.1 Create Todo Types Module** (`src/tools/todo.rs` - new file)

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// Status of a todo item
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl Default for TodoStatus {
    fn default() -> Self {
        TodoStatus::Pending
    }
}

impl std::fmt::Display for TodoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TodoStatus::Pending => write!(f, "pending"),
            TodoStatus::InProgress => write!(f, "in_progress"),
            TodoStatus::Completed => write!(f, "completed"),
            TodoStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// A single todo item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: usize,
    pub task: String,
    pub status: TodoStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The todo list manager
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TodoList {
    tasks: Vec<TodoItem>,
    next_id: usize,
}

impl TodoList {
    /// Create a new empty todo list
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            next_id: 1,
        }
    }

    /// Add a new task
    pub fn add(&mut self, task: String) -> TodoItem {
        let now = Utc::now();
        let item = TodoItem {
            id: self.next_id,
            task,
            status: TodoStatus::Pending,
            created_at: now,
            updated_at: now,
        };
        self.next_id += 1;
        self.tasks.push(item.clone());
        item
    }

    /// Update task status
    pub fn update_status(&mut self, id: usize, status: TodoStatus) -> Option<TodoItem> {
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            task.status = status;
            task.updated_at = Utc::now();
            return Some(task.clone());
        }
        None
    }

    /// Remove a task
    pub fn remove(&mut self, id: usize) -> Option<TodoItem> {
        if let Some(pos) = self.tasks.iter().position(|t| t.id == id) {
            Some(self.tasks.remove(pos))
        } else {
            None
        }
    }

    /// List all tasks
    pub fn list(&self) -> &[TodoItem] {
        &self.tasks
    }

    /// Clear completed tasks
    pub fn clear_completed(&mut self) -> usize {
        let initial_len = self.tasks.len();
        self.tasks.retain(|t| t.status != TodoStatus::Completed);
        initial_len - self.tasks.len()
    }

    /// Get task by ID
    pub fn get(&self, id: usize) -> Option<&TodoItem> {
        self.tasks.iter().find(|t| t.id == id)
    }
}
```

### Phase 2: Implement the Todo Tool

**2.1 Create TodoTool** (`src/tools/todo.rs`)

```rust
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::{Arc, Mutex};

/// Errors that can occur when using the todo tool
#[derive(Debug, thiserror::Error)]
pub enum TodoError {
    #[error("Task not found: {0}")]
    TaskNotFound(usize),
    
    #[error("Invalid action: {0}")]
    InvalidAction(String),
    
    #[error("Lock error: {0}")]
    LockError(String),
}

/// Arguments for the todo tool
#[derive(Deserialize)]
pub struct TodoArgs {
    /// The action to perform: add, update, remove, list, clear
    action: String,
    /// Task description (for add/update)
    #[serde(default)]
    task: Option<String>,
    /// Task status (for update): pending, in_progress, completed, cancelled
    #[serde(default)]
    status: Option<String>,
    /// Task ID (for update/remove)
    #[serde(default)]
    task_id: Option<usize>,
}

/// The todo tool
pub struct TodoTool {
    /// Shared todo list state
    todo_list: Arc<Mutex<TodoList>>,
}

impl TodoTool {
    /// Create a new todo tool
    pub fn new() -> Self {
        Self {
            todo_list: Arc::new(Mutex::new(TodoList::new())),
        }
    }

    /// Create with existing todo list (for testing)
    #[allow(dead_code)]
    pub fn with_list(todo_list: TodoList) -> Self {
        Self {
            todo_list: Arc::new(Mutex::new(todo_list)),
        }
    }

    /// Get the underlying Arc for sharing
    pub fn get_todo_list(&self) -> Arc<Mutex<TodoList>> {
        self.todo_list.clone()
    }
}

impl Default for TodoTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Output format for list action
#[derive(Serialize)]
struct TodoListOutput {
    tasks: Vec<TodoItemOutput>,
    summary: TodoSummary,
}

#[derive(Serialize)]
struct TodoItemOutput {
    id: usize,
    task: String,
    status: String,
    created_at: String,
}

#[derive(Serialize)]
struct TodoSummary {
    total: usize,
    pending: usize,
    in_progress: usize,
    completed: usize,
    cancelled: usize,
}

impl Tool for TodoTool {
    const NAME: &'static str = "todo";
    type Error = TodoError;
    type Args = TodoArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "todo".to_string(),
            description: "Manage a todo list to track progress on multi-step tasks. Use this to plan your approach, track progress, and keep the user informed about your work.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["add", "update", "remove", "list", "clear"],
                        "description": "The action to perform on the todo list"
                    },
                    "task": {
                        "type": "string",
                        "description": "Task description (required for add, optional for update)"
                    },
                    "status": {
                        "type": "string",
                        "enum": ["pending", "in_progress", "completed", "cancelled"],
                        "description": "Task status (for update action)"
                    },
                    "task_id": {
                        "type": "integer",
                        "description": "Task ID (required for update and remove actions)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let mut list = self.todo_list.lock()
            .map_err(|e| TodoError::LockError(e.to_string()))?;

        match args.action.as_str() {
            "add" => {
                let task = args.task.ok_or_else(|| {
                    TodoError::InvalidAction("Task description required for 'add' action".to_string())
                })?;
                let item = list.add(task);
                Ok(format!("Added task #{}: {}", item.id, item.task))
            }
            
            "update" => {
                let task_id = args.task_id.ok_or_else(|| {
                    TodoError::InvalidAction("task_id required for 'update' action".to_string())
                })?;
                let status_str = args.status.ok_or_else(|| {
                    TodoError::InvalidAction("status required for 'update' action".to_string())
                })?;
                
                let status = match status_str.as_str() {
                    "pending" => TodoStatus::Pending,
                    "in_progress" => TodoStatus::InProgress,
                    "completed" => TodoStatus::Completed,
                    "cancelled" => TodoStatus::Cancelled,
                    _ => return Err(TodoError::InvalidAction(format!("Invalid status: {}", status_str))),
                };
                
                match list.update_status(task_id, status) {
                    Some(item) => Ok(format!("Updated task #{} to {}", item.id, item.status)),
                    None => Err(TodoError::TaskNotFound(task_id)),
                }
            }
            
            "remove" => {
                let task_id = args.task_id.ok_or_else(|| {
                    TodoError::InvalidAction("task_id required for 'remove' action".to_string())
                })?;
                
                match list.remove(task_id) {
                    Some(item) => Ok(format!("Removed task #{}: {}", item.id, item.task)),
                    None => Err(TodoError::TaskNotFound(task_id)),
                }
            }
            
            "list" => {
                let tasks = list.list();
                let pending = tasks.iter().filter(|t| t.status == TodoStatus::Pending).count();
                let in_progress = tasks.iter().filter(|t| t.status == TodoStatus::InProgress).count();
                let completed = tasks.iter().filter(|t| t.status == TodoStatus::Completed).count();
                let cancelled = tasks.iter().filter(|t| t.status == TodoStatus::Cancelled).count();
                
                if tasks.is_empty() {
                    return Ok("No tasks in the todo list.".to_string());
                }
                
                let mut output = String::new();
                output.push_str("## Todo List\n\n");
                
                for item in tasks {
                    let status_icon = match item.status {
                        TodoStatus::Pending => "○",
                        TodoStatus::InProgress => "◐",
                        TodoStatus::Completed => "●",
                        TodoStatus::Cancelled => "✗",
                    };
                    output.push_str(&format!(
                        "{} #{} [{}] {}\n",
                        status_icon,
                        item.id,
                        item.status,
                        item.task
                    ));
                }
                
                output.push_str(&format!(
                    "\n**Summary:** {} pending, {} in progress, {} completed, {} cancelled",
                    pending, in_progress, completed, cancelled
                ));
                
                Ok(output)
            }
            
            "clear" => {
                let cleared = list.clear_completed();
                Ok(format!("Cleared {} completed tasks", cleared))
            }
            
            _ => Err(TodoError::InvalidAction(format!(
                "Unknown action: {}. Valid actions: add, update, remove, list, clear",
                args.action
            ))),
        }
    }
}
```

### Phase 2: Add Unit Tests (in same file)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_task() {
        let mut list = TodoList::new();
        let item = list.add("Test task".to_string());
        
        assert_eq!(item.id, 1);
        assert_eq!(item.task, "Test task");
        assert_eq!(item.status, TodoStatus::Pending);
    }

    #[test]
    fn test_update_status() {
        let mut list = TodoList::new();
        list.add("Test task".to_string());
        
        let updated = list.update_status(1, TodoStatus::InProgress);
        assert!(updated.is_some());
        assert_eq!(updated.unwrap().status, TodoStatus::InProgress);
    }

    #[test]
    fn test_remove_task() {
        let mut list = TodoList::new();
        list.add("Test task".to_string());
        
        let removed = list.remove(1);
        assert!(removed.is_some());
        assert!(list.get(1).is_none());
    }

    #[test]
    fn test_clear_completed() {
        let mut list = TodoList::new();
        list.add("Task 1".to_string());
        list.add("Task 2".to_string());
        
        list.update_status(1, TodoStatus::Completed).unwrap();
        
        let cleared = list.clear_completed();
        assert_eq!(cleared, 1);
        assert_eq!(list.list().len(), 1);
    }

    #[test]
    fn test_task_not_found() {
        let mut list = TodoList::new();
        
        let result = list.update_status(999, TodoStatus::Completed);
        assert!(result.is_none());
    }
}
```

### Phase 3: Register the Tool

**3.1 Update `src/tools/mod.rs`**

Add:
```rust
mod todo;
pub use todo::TodoTool;
```

**3.2 Update `src/providers/mod.rs`**

Add import:
```rust
use crate::tools::TodoTool;
```

Add to `add_builtin_tools`:
```rust
let builder = builder
    .tool(FileEditTool::default())
    .tool(FileReadTool)
    .tool(BashTool)
    .tool(ListDirectoryTool)
    .tool(FetchUrlTool)
    .tool(ThinkTool)
    .tool(TodoTool::default());  // Add this line
```

**3.3 Update `src/lib.rs` exports**

```rust
pub use tools::{
    BashTool, FetchUrlTool, FileEditTool, FileReadTool, ListDirectoryTool, LoggingToolDyn,
    SearchTool, ThinkTool, TodoTool,  // Add TodoTool
};
```

### Phase 4: System Prompt Updates

**4.1 Add to `system_prompt.txt`** (or dynamically in `build_system_prompt` in `lib.rs`)

Add a section about the todo tool:

```
## Using the todo tool

For multi-step tasks, use the todo tool to:
- Plan your approach by adding tasks
- Show the user what you intend to do
- Track progress as you complete each step
- Keep both yourself and the user informed

Actions:
- `todo add task="description"` - Add a new task
- `todo update task_id=1 status="in_progress"` - Update task status
- `todo remove task_id=1` - Remove a task
- `todo list` - Show all tasks with their status
- `todo clear` - Remove all completed tasks

Status values: "pending", "in_progress", "completed", "cancelled"

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

---

## Files to Modify

| File | Changes |
|------|---------|
| `src/tools/todo.rs` | **NEW** - Todo tool implementation |
| `src/tools/mod.rs` | Add `mod todo;` and `pub use todo::TodoTool;` |
| `src/providers/mod.rs` | Add `TodoTool` to imports and `add_builtin_tools` |
| `src/lib.rs` | Add `TodoTool` to public exports |
| `system_prompt.txt` | Add documentation section for todo tool |

---

## Dependencies

- No external dependencies required
- Uses existing `chrono` for timestamps (already in Cargo.toml)
- Uses existing `serde` and `serde_json` for serialization

---

## Testing Plan

1. **Unit Tests** (in `src/tools/todo.rs`):
   - Test adding tasks
   - Test updating task status
   - Test removing tasks
   - Test listing tasks
   - Test clearing completed tasks
   - Test error cases (task not found, invalid action, etc.)

2. **Manual Testing**:
   - Add several tasks
   - Update their statuses
   - List all tasks
   - Clear completed tasks
   - Verify state persists across tool calls

---

## Estimated Complexity

- **Time**: ~1-2 hours
- **Risk**: Low - isolated feature, no breaking changes
- **Testing**: Simple unit tests + manual testing

---

## User Visibility - How the User Sees the Todo List

The plan above handles the model's internal todo tracking. But users need to see the todo state prominently. Here's how we'll handle that:

### Architecture: Shared State with AgentRunner

The key insight is that the TodoTool maintains state via `Arc<Mutex<TodoList>>`. We can:
1. Create the TodoTool in `AgentRunner` (not inside the agent builder)
2. Store a reference to its shared state
3. After each model response, show a todo summary

### Implementation Approach

**1. Modify TodoTool to expose state:**
```rust
pub struct TodoTool {
    todo_list: Arc<Mutex<TodoList>>,
}

impl TodoTool {
    /// Get a clone of the Arc for sharing with AgentRunner
    pub fn get_state(&self) -> Arc<Mutex<TodoList>> {
        self.todo_list.clone()
    }
}
```

**2. Modify AgentRunner to track todo state:**
```rust
pub struct AgentRunner {
    // ... existing fields
    todo_state: Option<Arc<Mutex<TodoList>>>,  // NEW
}
```

**3. Show summary after each response:**
```rust
fn print_todo_summary(&self) {
    if let Some(ref state) = self.todo_state {
        if let Ok(list) = state.lock() {
            let tasks = list.list();
            let pending = tasks.iter().filter(|t| t.status == TodoStatus::Pending).count();
            let in_progress = tasks.iter().filter(|t| t.status == TodoStatus::InProgress).count();
            let completed = tasks.iter().filter(|t| t.status == TodoStatus::Completed).count();
            
            if !tasks.is_empty() {
                println!("\n[Todo: {} pending, {} in-progress, {} completed]\n", 
                    pending, in_progress, completed);
            }
        }
    }
}
```

Call `self.print_todo_summary()` after each successful response.

### What Users Will See

**After each model response:**
```
I'll investigate the login flow and fix the bug.

[Todo: 2 pending, 1 in-progress, 3 completed]
```

**When model explicitly calls `todo list`:**
```
> Fix the login bug

I'll create a plan for this.

[todo] Added task #1: Investigate login flow
[todo] Added task #2: Fix authentication logic

[Todo: 2 pending, 0 in-progress, 0 completed]
```

### Alternative: Verbose Mode (Optional)

If user wants full todo list visibility, we can also print the full list after each response. But the compact summary is probably better to avoid overwhelming users.

---

## Files Modified (Updated)

| File | Changes |
|------|---------|
| `src/tools/todo.rs` | **NEW** - Todo tool implementation + `get_state()` method |
| `src/tools/mod.rs` | Add `mod todo;` and `pub use todo::TodoTool;` |
| `src/providers/mod.rs` | Add `TodoTool` to imports, modify to accept optional pre-created tool with shared state |
| `src/lib.rs` | Add `TodoTool` to public exports, add `todo_state` field, add `print_todo_summary()` |
| `system_prompt.txt` | Add documentation section for todo tool |

### Detailed Flow

```
main.rs
  └── AgentRunner::new()
        │
        ├── Create TodoTool (Arc<Mutex<TodoList>>)
        │
        ├── Pass TodoTool to create_provider()
        │     │
        │     └── add_builtin_tools() uses the passed TodoTool
        │
        └── Store clone of TodoTool's Arc in AgentRunner.todo_state

After each response:
  AgentRunner::print_todo_summary() reads from todo_state and displays
```

### API Changes Required

**1. Update `create_provider` signature** in `src/providers/mod.rs`:

```rust
pub fn create_provider(
    config: &ProviderConfig,
    mcp_tools: Option<Vec<Box<dyn ToolDyn>>>,
    system_prompt: &str,
    searxng_config: Option<&SearXngConfig>,
    max_turns: usize,
    todo_tool: Option<TodoTool>,  // NEW: pass pre-created tool
) -> Result<(DynAgent, ProviderInfo, CostTracker, Option<Arc<Mutex<TodoList>>>)>  // Return state
```

**2. Update `add_builtin_tools`** to use passed TodoTool:

```rust
fn add_builtin_tools<M, P>(
    builder: ...,
    searxng_config: Option<&SearXngConfig>,
    todo_tool: Option<TodoTool>,  // NEW
) -> ...
```

**3. Update `main.rs`** to pass TodoTool and capture state:

```rust
let todo_tool = TodoTool::new();
let todo_state = todo_tool.get_state();

let (agent, provider_info, cost_tracker) = create_provider(
    &config.provider,
    mcp_tools,
    &system_prompt,
    config.searxng.as_ref(),
    max_turns,
    Some(todo_tool),  // Pass the tool
)?;

let runner = AgentRunner::new(
    agent,
    config,
    provider_info,
    skills,
    cost_tracker,
    Some(todo_state),  // Pass the shared state
);
```

---

## Original Task Reference

See `todo.md` Task 12 for full requirements.