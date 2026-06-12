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

    #[error("StateManager not set: {0}")]
    StateManagerNotSet(String),
}

/// The todo tool - a stateless controller that delegates to StateManager.
/// All todo state lives in StateManager; this tool just updates it.
#[derive(Default, Clone)]
pub struct TodoTool {
    /// Reference to StateManager (single source of truth for todo state)
    state_manager: Option<std::sync::Arc<StateManager>>,
}

impl TodoTool {
    /// Create a new todo tool with a StateManager reference
    pub fn new(state_manager: std::sync::Arc<StateManager>) -> Self {
        Self {
            state_manager: Some(state_manager),
        }
    }
}

/// Arguments for the todo tool
#[derive(Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub struct TodoArgs {
    /// Brief explanation of what you're doing
    #[allow(dead_code)]
    #[serde(default)]
    pub thought: String,
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
            thought: "Adding tasks".to_string(),
            action: "add".to_string(),
            tasks: Some(tasks),
            status: None,
            task_id: None,
        }
    }

    /// Create a new TodoArgs for listing tasks
    pub fn list() -> Self {
        Self {
            thought: "Listing tasks".to_string(),
            action: "list".to_string(),
            tasks: None,
            status: None,
            task_id: None,
        }
    }

    /// Create a new TodoArgs for updating a task
    pub fn update(id: usize, status: &str) -> Self {
        Self {
            thought: "Updating task".to_string(),
            action: "update".to_string(),
            tasks: None,
            status: Some(status.to_string()),
            task_id: Some(id),
        }
    }

    /// Create a new TodoArgs for removing a task
    pub fn remove(id: usize) -> Self {
        Self {
            thought: "Removing task".to_string(),
            action: "remove".to_string(),
            tasks: None,
            status: None,
            task_id: Some(id),
        }
    }

    /// Create a new TodoArgs for clearing completed tasks
    pub fn clear() -> Self {
        Self {
            thought: "Clearing completed".to_string(),
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
                    "thought": {
                        "type": "string",
                        "description": "Briefly explain what you're about to do and why, before acting."
                    },
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
                "required": ["thought", "action"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let sm = self.state_manager.as_ref().ok_or_else(|| {
            TodoError::StateManagerNotSet(
                "StateManager not initialized. Todo tool cannot update UI state.".to_string(),
            )
        })?;

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

                // Use add_todo for single task, add_todos for multiple
                // Both now return strings with info about new vs existing tasks
                if tasks.len() == 1 {
                    Ok(sm.add_todo(tasks.into_iter().next().unwrap()))
                } else {
                    Ok(sm.add_todos(tasks))
                }
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
                    _ => {
                        return Err(TodoError::InvalidAction(format!(
                            "Invalid status: {}",
                            status_str
                        )));
                    }
                };

                Ok(sm.update_todo_status(task_id, status))
            }

            "remove" => {
                let task_id = args.task_id.ok_or_else(|| {
                    TodoError::InvalidAction("task_id required for 'remove' action".to_string())
                })?;

                Ok(sm.remove_todo(task_id))
            }

            "list" => Ok(sm.list_todos()),

            "clear" => Ok(sm.clear_completed_todos()),

            _ => Err(TodoError::InvalidAction(format!(
                "Unknown action: {}. Valid actions: add, update, remove, list, clear",
                args.action
            ))),
        }
    }
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
