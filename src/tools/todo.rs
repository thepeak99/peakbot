//! Todo tool - allows the model to track progress on multi-step tasks.

use crate::state::StateManager;
use chrono::{DateTime, Utc};
use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;

// Domain types (defined here, exported for use by StateManager and other modules)

/// Status of a todo item
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TodoStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Cancelled,
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

/// Result of adding a task - either newly added or already exists
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddTaskResult {
    pub id: usize,
    pub task: String,
    pub is_new: bool,
}

/// The todo list manager
#[derive(Debug, Clone, Serialize, Default)]
pub struct TodoList {
    tasks: Vec<TodoItem>,
    next_id: usize,
}

/// Custom deserializer for [`TodoList`] that accepts both the current
/// struct shape (`{"tasks":[…],"next_id":N}`) and the legacy array shape
/// (`[…]`). Pre-todo-list conversation files serialised the slot as an
/// empty array; a strict struct deserializer 400s on those (see contract 8
/// in `tests/scenarios/reasoning_preservation.rs`). The legacy array is
/// treated as an empty list with `next_id = 1` — there's nothing else
/// it could meaningfully mean.
impl<'de> Deserialize<'de> for TodoList {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Shape {
            /// Legacy / stub form: a JSON array. Either empty (the common
            /// pre-todo file) or a sequence of `TodoItem` we accept
            /// verbatim (best-effort; `next_id` is inferred from `max(id)+1`).
            Legacy(Vec<TodoItem>),
            /// Current form: a struct with both fields.
            Struct {
                tasks: Vec<TodoItem>,
                next_id: usize,
            },
        }

        let shape = Shape::deserialize(deserializer)?;
        match shape {
            Shape::Legacy(items) => {
                let next_id = items.iter().map(|t| t.id).max().unwrap_or(0) + 1;
                Ok(TodoList {
                    tasks: items,
                    next_id,
                })
            }
            Shape::Struct { tasks, next_id } => Ok(TodoList { tasks, next_id }),
        }
    }
}

impl TodoList {
    /// Create a new empty todo list
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            next_id: 1,
        }
    }

    /// Add a new task (returns AddTaskResult indicating if new or already existed)
    /// Returns the task info with is_new=false if duplicate exists (case-insensitive)
    pub fn add(&mut self, task: String) -> AddTaskResult {
        let task_lower = task.to_lowercase();

        // Check if task already exists (case-insensitive)
        if let Some(existing) = self
            .tasks
            .iter()
            .find(|t| t.task.to_lowercase() == task_lower)
        {
            return AddTaskResult {
                id: existing.id,
                task: existing.task.clone(),
                is_new: false,
            };
        }

        let now = Utc::now();
        let id = self.next_id;
        let item = TodoItem {
            id,
            task,
            status: TodoStatus::Pending,
            created_at: now,
            updated_at: now,
        };
        self.next_id += 1;
        self.tasks.push(item.clone());

        AddTaskResult {
            id,
            task: item.task,
            is_new: true,
        }
    }

    /// Add multiple tasks at once
    /// Returns results for all tasks (both new and existing)
    pub fn add_many(&mut self, tasks: Vec<String>) -> Vec<AddTaskResult> {
        tasks.into_iter().map(|task| self.add(task)).collect()
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

    /// Clear finished tasks (both completed and cancelled).
    /// If the list becomes empty, resets the internal id counter so new
    /// tasks start from 1 again.
    pub fn clear_completed(&mut self) -> usize {
        let initial_len = self.tasks.len();
        self.tasks
            .retain(|t| t.status != TodoStatus::Completed && t.status != TodoStatus::Cancelled);
        if self.tasks.is_empty() {
            self.next_id = 1;
        }
        initial_len - self.tasks.len()
    }

    /// Get task by ID
    pub fn get(&self, id: usize) -> Option<&TodoItem> {
        self.tasks.iter().find(|t| t.id == id)
    }

    /// Count tasks by status
    pub fn count_by_status(&self) -> (usize, usize, usize, usize) {
        let pending = self
            .tasks
            .iter()
            .filter(|t| t.status == TodoStatus::Pending)
            .count();
        let in_progress = self
            .tasks
            .iter()
            .filter(|t| t.status == TodoStatus::InProgress)
            .count();
        let completed = self
            .tasks
            .iter()
            .filter(|t| t.status == TodoStatus::Completed)
            .count();
        let cancelled = self
            .tasks
            .iter()
            .filter(|t| t.status == TodoStatus::Cancelled)
            .count();
        (pending, in_progress, completed, cancelled)
    }

    // ── Mutate-and-render helpers ───────────────────────────────────
    // These own the user-facing strings the `todo` tool returns. `StateManager`
    // delegates to them and layers its UI side-effects (panel show / sync) on
    // top; a sub-agent's standalone `TodoTool` calls them directly on its own
    // isolated list. One home for the format strings — necessarily-same for
    // both backends.

    /// Add a single task, returning the tool-facing status line.
    pub fn add_one(&mut self, task: String) -> String {
        let result = self.add(task);
        if result.is_new {
            format!("Added task #{}: {}", result.id, result.task)
        } else {
            format!("Task already exists as #{}: {}", result.id, result.task)
        }
    }

    /// Add several tasks, returning a summary of new vs. already-existing.
    pub fn add_batch(&mut self, tasks: Vec<String>) -> String {
        if tasks.is_empty() {
            return "No tasks provided.".to_string();
        }
        let results = self.add_many(tasks);
        let new_tasks: Vec<_> = results.iter().filter(|r| r.is_new).collect();
        let existing_tasks: Vec<_> = results.iter().filter(|r| !r.is_new).collect();

        let mut output = String::new();
        if !new_tasks.is_empty() {
            let new_list: Vec<String> = new_tasks
                .iter()
                .map(|r| format!("#{}: {}", r.id, r.task))
                .collect();
            output.push_str(&format!(
                "Added {} task(s): {}\n",
                new_tasks.len(),
                new_list.join(", ")
            ));
        }
        if !existing_tasks.is_empty() {
            let existing_list: Vec<String> = existing_tasks
                .iter()
                .map(|r| format!("#{}: {}", r.id, r.task))
                .collect();
            output.push_str(&format!("Already existed: {}", existing_list.join(", ")));
        }
        output.trim().to_string()
    }

    /// Update a task's status, returning the tool-facing status line.
    pub fn update_one(&mut self, id: usize, status: TodoStatus) -> String {
        match self.update_status(id, status) {
            Some(item) => format!("Updated task #{} to {}", item.id, item.status),
            None => format!("Task #{} not found", id),
        }
    }

    /// Remove a task, returning the tool-facing status line.
    pub fn remove_one(&mut self, id: usize) -> String {
        match self.remove(id) {
            Some(item) => format!("Removed task #{}: {}", item.id, item.task),
            None => format!("Task #{} not found", id),
        }
    }

    /// Clear finished tasks, returning the tool-facing status line.
    pub fn clear_finished(&mut self) -> String {
        let cleared = self.clear_completed();
        format!("Cleared {cleared} finished tasks (completed and cancelled)")
    }

    /// Render the full list as the markdown block the `todo` tool returns.
    pub fn render(&self) -> String {
        let tasks = self.list();
        if tasks.is_empty() {
            return "No tasks in the todo list.".to_string();
        }
        let (pending, in_progress, completed, cancelled) = self.count_by_status();

        let mut output = String::new();
        output.push_str("## Todo List\n\n");
        for item in tasks {
            // Glyph palette mirrors `ui::repl::todo_panel`: `x` (ASCII) for
            // cancelled to avoid kitty double-width column drift — see
            // `garbled.md` Class B.
            let status_icon = match item.status {
                TodoStatus::Pending => "○",
                TodoStatus::InProgress => "◐",
                TodoStatus::Completed => "●",
                TodoStatus::Cancelled => "x",
            };
            output.push_str(&format!(
                "{} #{} [{}] {}\n",
                status_icon, item.id, item.status, item.task
            ));
        }
        output.push_str(&format!(
            "\n**Summary:** {} pending, {} in progress, {} completed, {} cancelled",
            pending, in_progress, completed, cancelled
        ));
        output
    }
}

/// Errors that can occur when using the todo tool
#[derive(Debug, thiserror::Error)]
pub enum TodoError {
    #[error("Task not found: {0}")]
    TaskNotFound(usize),

    #[error("Invalid action: {0}")]
    InvalidAction(String),

    #[error("Task already exists: {0}")]
    DuplicateTask(String),
}

/// The todo tool — a stateless controller.
///
/// Two backends:
/// - [`TodoBackend::Panel`]: the orchestrator's tool. Delegates to the
///   session `StateManager`, which owns the visible todo panel and drives its
///   UI side-effects (auto-show, sync).
/// - [`TodoBackend::Standalone`]: a sub-agent's tool. Owns a fresh isolated
///   `TodoList` and drives no panel — the same graceful-degradation shape
///   `bash`/`bash_bg` use when there's no `StateManager`. The panel is an
///   orchestrator-only affordance.
///
/// `Default` is the standalone backend with a fresh list, so a sub-agent that
/// reaches this tool via `todo_tool.unwrap_or_default()` gets a *working*,
/// isolated todo — not the old broken `state_manager: None` tool that
/// hard-errored on every call.
#[derive(Clone)]
pub struct TodoTool {
    backend: TodoBackend,
}

#[derive(Clone)]
enum TodoBackend {
    /// Orchestrator: delegate to StateManager (drives the visible panel).
    Panel(std::sync::Arc<StateManager>),
    /// Sub-agent: an isolated in-memory list, no panel.
    Standalone(std::sync::Arc<std::sync::Mutex<TodoList>>),
}

impl Default for TodoTool {
    fn default() -> Self {
        Self {
            backend: TodoBackend::Standalone(std::sync::Arc::new(std::sync::Mutex::new(
                TodoList::new(),
            ))),
        }
    }
}

impl TodoTool {
    /// Create the orchestrator's panel-backed todo tool.
    pub fn new(state_manager: std::sync::Arc<StateManager>) -> Self {
        Self {
            backend: TodoBackend::Panel(state_manager),
        }
    }
}

/// Arguments for the todo tool
#[derive(Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub struct TodoArgs {
    /// The action to perform: add, update, remove, list, clear
    pub action: String,
    /// Task descriptions (for add)
    #[serde(default)]
    pub tasks: Option<Vec<String>>,
    /// Task status (for update): pending, in_progress, completed, cancelled
    #[serde(default)]
    pub status: Option<String>,
    /// Task ID (for update/remove)
    #[serde(default)]
    pub task_id: Option<usize>,
}

impl TodoArgs {
    /// Create a new TodoArgs for adding tasks
    pub fn add(tasks: Vec<String>) -> Self {
        Self {
            action: "add".to_string(),
            tasks: Some(tasks),
            status: None,
            task_id: None,
        }
    }

    /// Create a new TodoArgs for listing tasks
    pub fn list() -> Self {
        Self {
            action: "list".to_string(),
            tasks: None,
            status: None,
            task_id: None,
        }
    }

    /// Create a new TodoArgs for updating a task
    pub fn update(id: usize, status: &str) -> Self {
        Self {
            action: "update".to_string(),
            tasks: None,
            status: Some(status.to_string()),
            task_id: Some(id),
        }
    }

    /// Create a new TodoArgs for removing a task
    pub fn remove(id: usize) -> Self {
        Self {
            action: "remove".to_string(),
            tasks: None,
            status: None,
            task_id: Some(id),
        }
    }

    /// Create a new TodoArgs for clearing completed tasks
    pub fn clear() -> Self {
        Self {
            action: "clear".to_string(),
            tasks: None,
            status: None,
            task_id: None,
        }
    }
}

impl Tool for TodoTool {
    const NAME: &'static str = "todo";
    type Error = TodoError;
    type Args = TodoArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "todo".to_string(),
            description: "Manage a todo list to track progress on multi-step tasks. Use this to plan your approach, track progress, and keep the user informed about your work. For the 'add' action, always use the 'tasks' array parameter (even for a single task, use an array with one element).".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["add", "update", "remove", "list", "clear"],
                        "description": "The action to perform on the todo list"
                    },
                    "tasks": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Array of task descriptions (required for 'add' action, use array even for single task)"
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
        match &self.backend {
            TodoBackend::Panel(sm) => Self::call_panel(sm, args),
            TodoBackend::Standalone(list) => Self::call_standalone(list, args),
        }
    }
}

impl TodoTool {
    /// Orchestrator backend: delegate to StateManager so the visible panel
    /// updates. StateManager owns the UI side-effects; rendering lives on
    /// `TodoList` (shared with the standalone backend).
    fn call_panel(sm: &StateManager, args: TodoArgs) -> Result<String, TodoError> {
        match args.action.as_str() {
            "add" => {
                let tasks = args.tasks.ok_or_else(|| {
                    TodoError::InvalidAction("Tasks array required for 'add' action".to_string())
                })?;
                if tasks.is_empty() {
                    return Err(TodoError::InvalidAction(
                        "Tasks array is empty for 'add' action".to_string(),
                    ));
                }
                if tasks.len() == 1 {
                    Ok(sm.add_todo(tasks.into_iter().next().unwrap()))
                } else {
                    Ok(sm.add_todos(tasks))
                }
            }
            "update" => {
                let (id, status) = Self::parse_update(&args)?;
                Ok(sm.update_todo_status(id, status))
            }
            "remove" => Ok(sm.remove_todo(Self::parse_id(&args, "remove")?)),
            "list" => Ok(sm.list_todos()),
            "clear" => Ok(sm.clear_completed_todos()),
            other => Err(unknown_action(other)),
        }
    }

    /// Sub-agent backend: an isolated in-memory list, no panel. Uses the same
    /// `TodoList` render/mutate helpers as the panel path — identical output,
    /// no shared state with the orchestrator.
    fn call_standalone(
        list: &std::sync::Mutex<TodoList>,
        args: TodoArgs,
    ) -> Result<String, TodoError> {
        let mut list = list.lock().unwrap();
        match args.action.as_str() {
            "add" => {
                let tasks = args.tasks.ok_or_else(|| {
                    TodoError::InvalidAction("Tasks array required for 'add' action".to_string())
                })?;
                if tasks.is_empty() {
                    return Err(TodoError::InvalidAction(
                        "Tasks array is empty for 'add' action".to_string(),
                    ));
                }
                if tasks.len() == 1 {
                    Ok(list.add_one(tasks.into_iter().next().unwrap()))
                } else {
                    Ok(list.add_batch(tasks))
                }
            }
            "update" => {
                let (id, status) = Self::parse_update(&args)?;
                Ok(list.update_one(id, status))
            }
            "remove" => Ok(list.remove_one(Self::parse_id(&args, "remove")?)),
            "list" => Ok(list.render()),
            "clear" => Ok(list.clear_finished()),
            other => Err(unknown_action(other)),
        }
    }

    fn parse_id(args: &TodoArgs, action: &str) -> Result<usize, TodoError> {
        args.task_id.ok_or_else(|| {
            TodoError::InvalidAction(format!("task_id required for '{action}' action"))
        })
    }

    fn parse_update(args: &TodoArgs) -> Result<(usize, TodoStatus), TodoError> {
        let id = Self::parse_id(args, "update")?;
        let status_str = args.status.as_ref().ok_or_else(|| {
            TodoError::InvalidAction("status required for 'update' action".to_string())
        })?;
        let status = match status_str.as_str() {
            "pending" => TodoStatus::Pending,
            "in_progress" => TodoStatus::InProgress,
            "completed" => TodoStatus::Completed,
            "cancelled" => TodoStatus::Cancelled,
            other => {
                return Err(TodoError::InvalidAction(format!("Invalid status: {other}")));
            }
        };
        Ok((id, status))
    }
}

/// Shared "unknown action" error for both backends.
fn unknown_action(action: &str) -> TodoError {
    TodoError::InvalidAction(format!(
        "Unknown action: {action}. Valid actions: add, update, remove, list, clear"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_task() {
        let mut list = TodoList::new();
        let result = list.add("Test task".to_string());

        assert!(result.is_new);
        assert_eq!(result.id, 1);
        assert_eq!(result.task, "Test task");
    }

    // ── standalone (sub-agent) todo is functional AND isolated ────────────────

    /// The default `TodoTool` (a sub-agent's, via `unwrap_or_default()`) works
    /// end-to-end — `add` then `list` succeed without the old
    /// `StateManager not set` error — and each default tool owns an *isolated*
    /// list, so one sub-agent's todos never appear in another's.
    #[tokio::test]
    async fn standalone_todo_tool_works_and_is_isolated() {
        let a = TodoTool::default();
        let b = TodoTool::default();

        // `a` adds a task — must succeed (no StateManager error).
        let added = a
            .call(TodoArgs::add(vec!["alpha-only-task".to_string()]))
            .await
            .expect("standalone add must not error");
        assert!(
            added.contains("alpha-only-task"),
            "add echoes the task: {added}"
        );

        // `a` lists it back.
        let listed_a = a.call(TodoArgs::list()).await.expect("list must not error");
        assert!(listed_a.contains("alpha-only-task"));

        // `b` is a *separate* list — `a`'s task is not visible in `b`.
        let listed_b = b.call(TodoArgs::list()).await.expect("list must not error");
        assert!(
            !listed_b.contains("alpha-only-task"),
            "each standalone tool must own an isolated list; got: {listed_b}"
        );
        assert!(listed_b.contains("No tasks"), "b starts empty: {listed_b}");
    }

    #[test]
    fn test_add_duplicate_returns_existing() {
        let mut list = TodoList::new();
        let first = list.add("Test task".to_string());
        assert_eq!(first.id, 1);
        assert!(first.is_new);

        let second = list.add("Test task".to_string());
        assert!(!second.is_new);
        assert_eq!(second.id, 1);
        assert_eq!(second.task, "Test task");
    }

    #[test]
    fn test_add_duplicate_case_insensitive() {
        let mut list = TodoList::new();
        list.add("Test task".to_string());

        let result = list.add("test task".to_string());
        assert!(!result.is_new);
        assert_eq!(result.id, 1);
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
    fn test_clear_also_removes_cancelled_tasks() {
        let mut list = TodoList::new();
        list.add("Task 1".to_string());
        list.add("Task 2".to_string());
        list.add("Task 3".to_string());

        list.update_status(1, TodoStatus::Completed).unwrap();
        list.update_status(2, TodoStatus::Cancelled).unwrap();
        // Task 3 stays pending

        let cleared = list.clear_completed();
        assert_eq!(
            cleared, 2,
            "clear should remove both completed and cancelled"
        );
        assert_eq!(list.list().len(), 1);
        assert_eq!(list.list()[0].id, 3);
    }

    #[test]
    fn test_clear_resets_next_id_when_list_empty() {
        let mut list = TodoList::new();
        list.add("Task 1".to_string());
        list.add("Task 2".to_string());
        list.update_status(1, TodoStatus::Completed).unwrap();
        list.update_status(2, TodoStatus::Cancelled).unwrap();

        list.clear_completed();
        assert_eq!(list.list().len(), 0);

        // After clearing an emptied list, IDs should restart from 1
        let result = list.add("Fresh task".to_string());
        assert_eq!(result.id, 1, "next_id should reset when list becomes empty");
    }

    #[test]
    fn test_clear_does_not_reset_next_id_if_tasks_remain() {
        let mut list = TodoList::new();
        list.add("Task 1".to_string());
        list.add("Task 2".to_string());
        list.update_status(1, TodoStatus::Completed).unwrap();

        list.clear_completed();
        // Task 2 still there with id=2, so next id must be 3 (not colliding)
        let result = list.add("Task 3".to_string());
        assert_eq!(result.id, 3);
    }

    #[test]
    fn test_task_not_found() {
        let mut list = TodoList::new();

        let result = list.update_status(999, TodoStatus::Completed);
        assert!(result.is_none());
    }

    #[test]
    fn test_count_by_status() {
        let mut list = TodoList::new();
        list.add("Task 1".to_string());
        list.add("Task 2".to_string());
        list.add("Task 3".to_string());

        list.update_status(1, TodoStatus::Pending).unwrap();
        list.update_status(2, TodoStatus::InProgress).unwrap();
        list.update_status(3, TodoStatus::Completed).unwrap();

        let (pending, in_progress, completed, cancelled) = list.count_by_status();
        assert_eq!(pending, 1);
        assert_eq!(in_progress, 1);
        assert_eq!(completed, 1);
        assert_eq!(cancelled, 0);
    }

    #[test]
    fn test_add_many_tasks() {
        let mut list = TodoList::new();
        let results = list.add_many(vec![
            "Task 1".to_string(),
            "Task 2".to_string(),
            "Task 3".to_string(),
        ]);

        assert_eq!(results.len(), 3);
        assert!(results[0].is_new);
        assert!(results[1].is_new);
        assert!(results[2].is_new);
        assert_eq!(results[0].id, 1);
        assert_eq!(results[1].id, 2);
        assert_eq!(results[2].id, 3);
        assert_eq!(list.list().len(), 3);
    }

    #[test]
    fn test_add_many_with_duplicates() {
        let mut list = TodoList::new();
        list.add("Task 1".to_string());

        let results = list.add_many(vec![
            "Task 1".to_string(), // duplicate
            "Task 2".to_string(), // new
        ]);

        assert_eq!(results.len(), 2);
        assert!(!results[0].is_new); // Task 1 was already there
        assert!(results[1].is_new); // Task 2 is new
        assert_eq!(results[0].id, 1); // existing id
        assert_eq!(results[1].id, 2); // new id
        assert_eq!(list.list().len(), 2); // only Task 1 and Task 2
    }

    #[test]
    fn test_add_many_single_task() {
        let mut list = TodoList::new();
        let results = list.add_many(vec!["Single task".to_string()]);

        assert_eq!(results.len(), 1);
        assert!(results[0].is_new);
        assert_eq!(results[0].id, 1);
    }
}
