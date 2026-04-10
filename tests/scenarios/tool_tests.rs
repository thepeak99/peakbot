//! Tool tests - Direct TodoTool testing
//!
//! Tests for verifying TodoTool works correctly when called directly.
//! No mock needed - we test the real tool with real StateManager.

use peakbot::state::StateManager;
use peakbot::{TodoArgs, TodoStatus, TodoTool};
use rig::tool::Tool;
use std::sync::Arc;

#[tokio::test]
async fn todo_add_item_direct() {
    let state_manager = Arc::new(StateManager::new());
    let todo_tool = TodoTool::new(state_manager.clone());

    // Call the actual tool using builder method
    let result = todo_tool
        .call(TodoArgs::add(vec!["Fix bug".to_string()]))
        .await;

    assert!(result.is_ok());

    // Verify StateManager state changed
    let todo_list = state_manager.get_todo_list();
    let items = todo_list.list();

    assert_eq!(items.len(), 1, "Should have 1 todo item");
    assert_eq!(items[0].task, "Fix bug", "Task text should match");
    assert_eq!(
        items[0].status,
        TodoStatus::Pending,
        "Status should be Pending"
    );
}

#[tokio::test]
async fn todo_add_multiple_items() {
    let state_manager = Arc::new(StateManager::new());
    let todo_tool = TodoTool::new(state_manager.clone());

    let result = todo_tool
        .call(TodoArgs::add(vec![
            "Task one".to_string(),
            "Task two".to_string(),
            "Task three".to_string(),
        ]))
        .await;

    assert!(result.is_ok());

    let todo_list = state_manager.get_todo_list();
    let items = todo_list.list();

    assert_eq!(items.len(), 3, "Should have 3 todo items");
    assert_eq!(items[0].task, "Task one");
    assert_eq!(items[1].task, "Task two");
    assert_eq!(items[2].task, "Task three");
}

#[tokio::test]
async fn todo_update_status() {
    let state_manager = Arc::new(StateManager::new());
    let todo_tool = TodoTool::new(state_manager.clone());

    // First add a task
    todo_tool
        .call(TodoArgs::add(vec!["Test task".to_string()]))
        .await
        .unwrap();

    // Update the task status
    let result = todo_tool.call(TodoArgs::update(1, "completed")).await;
    assert!(result.is_ok());

    let todo_list = state_manager.get_todo_list();
    let items = todo_list.list();
    assert_eq!(items[0].status, TodoStatus::Completed);
}

#[tokio::test]
async fn todo_remove_item() {
    let state_manager = Arc::new(StateManager::new());
    let todo_tool = TodoTool::new(state_manager.clone());

    // Add tasks
    todo_tool
        .call(TodoArgs::add(vec!["Task to remove".to_string()]))
        .await
        .unwrap();

    // Remove the task
    let result = todo_tool.call(TodoArgs::remove(1)).await;
    assert!(result.is_ok());

    let todo_list = state_manager.get_todo_list();
    let items = todo_list.list();
    assert!(items.is_empty());
}

#[tokio::test]
async fn todo_list_items() {
    let state_manager = Arc::new(StateManager::new());
    let todo_tool = TodoTool::new(state_manager.clone());

    // Add some tasks
    todo_tool
        .call(TodoArgs::add(vec!["First task".to_string()]))
        .await
        .unwrap();

    // List them
    let result = todo_tool.call(TodoArgs::list()).await;
    assert!(result.is_ok());

    let output = result.unwrap();
    assert!(output.contains("First task"));
}

#[tokio::test]
async fn todo_clear_completed() {
    let state_manager = Arc::new(StateManager::new());
    let todo_tool = TodoTool::new(state_manager.clone());

    // Add and complete a task
    todo_tool
        .call(TodoArgs::add(vec!["Completed task".to_string()]))
        .await
        .unwrap();
    todo_tool
        .call(TodoArgs::update(1, "completed"))
        .await
        .unwrap();

    // Clear completed
    let result = todo_tool.call(TodoArgs::clear()).await;
    assert!(result.is_ok());

    let todo_list = state_manager.get_todo_list();
    let items = todo_list.list();
    assert!(items.is_empty());
}

#[tokio::test]
async fn todo_update_nonexistent() {
    let state_manager = Arc::new(StateManager::new());
    let todo_tool = TodoTool::new(state_manager.clone());

    // Try to update nonexistent task
    let result = todo_tool.call(TodoArgs::update(999, "completed")).await;
    // Should fail gracefully
    assert!(result.is_err() || result.unwrap().contains("not found"));
}
